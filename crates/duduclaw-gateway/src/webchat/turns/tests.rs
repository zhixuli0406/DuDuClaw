use super::*;

#[tokio::test]
async fn sessions_run_concurrently_and_same_session_is_fifo_across_sockets() {
    let registry = Arc::new(Registry::default());
    let first = registry
        .admit("alice", "a", Some("a".into()), "1", 3, 10)
        .unwrap();
    let mut second = registry
        .admit("alice", "a", Some("a".into()), "2", 3, 10)
        .unwrap();
    let other = registry
        .admit("alice", "b", Some("b".into()), "3", 3, 10)
        .unwrap();
    assert!(first.predecessor.is_none());
    assert!(other.predecessor.is_none());
    let predecessor = second.predecessor.as_mut().unwrap();
    assert_eq!(
        predecessor.try_recv(),
        Err(oneshot::error::TryRecvError::Empty)
    );
    drop(first);
    assert!(second.predecessor.take().unwrap().await.is_ok());
    drop(second);
    drop(other);
    assert!(registry.entries.lock().unwrap().is_empty());
}

#[test]
fn cap_is_per_authenticated_user_and_counts_queued_turns() {
    let registry = Arc::new(Registry::default());
    let _a = registry.admit("alice", "socket1-a", None, "1", 2, 10).unwrap();
    let _b = registry.admit("alice", "socket2-b", None, "2", 2, 10).unwrap();
    assert!(matches!(
        registry.admit("alice", "socket2-c", None, "3", 2, 10),
        Err("turn_limit")
    ));
    assert!(registry.admit("bob", "socket3-a", None, "1", 2, 10).is_ok());
}

#[tokio::test]
async fn cancel_is_owned_exact_and_refuses_ambiguous_or_delivery_turns() {
    let registry = Arc::new(Registry::default());
    let a = registry
        .admit("alice", "a", Some("a".into()), "1", 3, 10)
        .unwrap();
    let b = registry
        .admit("alice", "a", Some("a".into()), "2", 3, 10)
        .unwrap();
    assert!(registry.cancel("bob", Some("a"), None, Some("1"), None).is_err());
    assert!(
        registry
            .cancel("alice", Some("missing"), None, None, None)
            .is_err()
    );
    assert!(registry.cancel("alice", Some("a"), None, None, None).is_ok());
    assert!(registry.cancel("alice", None, Some("a"), Some("1"), None).is_err());
    assert!(a.cancel.is_cancelled());
    assert!(!b.cancel.is_cancelled());
    assert!(!a.begin_delivery());
    assert!(b.begin_delivery());
    assert!(
        registry
            .cancel("alice", Some("a"), None, Some("2"), None)
            .is_err()
    );
}

#[test]
fn config_defaults_and_invalid_values_fail_closed() {
    let home = tempfile::tempdir().unwrap();
    assert_eq!(configured_limit(home.path()), 3);
    for (value, expected) in [("2", 2), ("0", 0), ("65", 0), ("'bad'", 0)] {
        std::fs::write(
            home.path().join("config.toml"),
            format!("[webchat]\nmax_in_flight_turns={value}\n"),
        )
        .unwrap();
        assert_eq!(configured_limit(home.path()), expected);
    }
}

#[tokio::test]
async fn outbound_frames_keep_correlation_and_wait_for_actual_send() {
    let (tx, mut rx) = outbound_channel();
    let mut sink = TurnSink::new(tx, "session", Some("conv".into()), "message").acknowledged();
    let task = tokio::spawn(async move {
        sink.send(Message::Text(
            r#"{"type":"assistant_done","content":"ok","tokens_used":0}"#.into(),
        ))
        .await
    });
    let frame = rx.recv().await.unwrap();
    assert!(!task.is_finished());
    let Message::Text(text) = frame.message else {
        panic!("expected text")
    };
    let value: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(value["session_id"], "session");
    assert_eq!(value["conv"], "conv");
    assert_eq!(value["msg_id"], "message");
    frame.ack.unwrap().send(true).unwrap();
    assert!(task.await.unwrap().is_ok());
}

#[cfg(unix)]
#[tokio::test]
async fn cancel_kills_process_records_session_and_withdraws_own_approval() {
    let home = tempfile::tempdir().unwrap();
    let sessions = crate::session::SessionManager::new(&home.path().join("sessions.db")).unwrap();
    sessions.get_or_create("session", "bot").await.unwrap();
    let broker = crate::approval::ApprovalBroker::open(home.path()).unwrap();
    let other = crate::approval::scope_turn(
        "other",
        "other",
        broker.request("bot", "test", "other", serde_json::Value::Null, 600),
    )
    .await
    .unwrap();
    let registry = Arc::new(Registry::default());
    let ticket = registry
        .admit("alice", "session", None, "message", 3, 10)
        .unwrap();
    let id = ticket.id.clone();
    let (ready, started) = oneshot::channel();
    let task = tokio::spawn(async move {
        let (cancelled, cleanup) = run_cancellable("session", &ticket, async {
            let approval = broker
                .request("bot", "test", "own", serde_json::Value::Null, 600)
                .await
                .unwrap();
            let mut command = tokio::process::Command::new("sleep");
            command.arg("60").kill_on_drop(true);
            let (mut child, _group) = crate::process_group::spawn_guarded(&mut command).unwrap();
            ready.send((child.id().unwrap(), approval)).unwrap();
            let _ = child.wait().await;
        })
        .await;
        assert!(cancelled);
        cleanup.unwrap();
        ticket
    });
    let (pid, approval) = started.await.unwrap();
    registry
        .cancel("alice", Some("session"), None, Some("message"), None)
        .unwrap();
    let ticket = task.await.unwrap();
    let queue = crate::message_queue::MessageQueue::open(home.path()).unwrap();
    let conn = rusqlite::Connection::open(home.path().join("message_queue.db")).unwrap();
    conn.execute("INSERT INTO message_queue(id,sender,target,payload,created_at,session_id,turn_id)
        VALUES ('delegation','bot','other','work',datetime('now'),'session',?1)", [&ticket.id]).unwrap();
    let (tx, mut rx) = outbound_channel();
    let sink = TurnSink::new(tx, "session", None, "message");
    finish_cancel(home.path(), Some(&crate::approval::ApprovalBroker::open(home.path()).unwrap()), &sessions, "session", "bot", &ticket, &sink, Ok(())).await;
    let broker = crate::approval::ApprovalBroker::open(home.path()).unwrap();
    let own = broker.get(&approval).await.unwrap().unwrap();
    assert_eq!(own.status, crate::approval::ApprovalStatus::Denied);
    assert_eq!(
        own.decided_by.as_deref(),
        Some("system:cancel:webchat_turn")
    );
    assert_eq!(
        broker.get(&other).await.unwrap().unwrap().status,
        crate::approval::ApprovalStatus::Pending
    );
    assert!(sessions.get_messages("session").await.unwrap().is_empty());
    let stored: String = sessions.acquire().await.query_row(
        "SELECT turn_id FROM webchat_turn_cancellations WHERE session_id='session'", [], |r| r.get(0),
    ).unwrap();
    assert_eq!(stored, id);
    let frame = rx.recv().await.unwrap();
    let Message::Text(text) = frame.message else {
        panic!("expected text")
    };
    let terminal: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(terminal["code"], "cancelled");
    assert_eq!(terminal["delegations_cancelled"], 1);
    assert!(queue.pending_messages(10).await.unwrap().is_empty());
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let alive = std::process::Command::new("kill")
                .args(["-0", &pid.to_string()])
                .output()
                .unwrap()
                .status
                .success();
            if !alive {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("cancelled process must terminate");
    drop(ticket);
}

#[tokio::test]
async fn cancellation_cleanup_failure_never_reports_success() {
    let home = tempfile::tempdir().unwrap();
    let sessions = crate::session::SessionManager::new(&home.path().join("sessions.db")).unwrap();
    sessions
        .get_or_create("session", "actual-owner")
        .await
        .unwrap();
    let (tx, mut rx) = outbound_channel();
    let sink = TurnSink::new(tx, "session", None, "message");
    let registry = Arc::new(Registry::default());
    let ticket = registry.admit("alice", "session", None, "message", 3, 10).unwrap();
    ticket.started.store(true, Ordering::Release);
    finish_cancel(home.path(),
        Some(&crate::approval::ApprovalBroker::open(home.path()).unwrap()),
        &sessions,
        "session",
        "bot",
        &ticket,
        &sink,
        Err("injected row revocation failure".into()),
    )
    .await;
    let Message::Text(text) = rx.recv().await.unwrap().message else {
        panic!("expected text")
    };
    let value: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(value["code"], "cancel_failed");
    assert_eq!(
        sessions.session_agent("session").await.unwrap().as_deref(),
        Some("actual-owner")
    );
    assert!(
        sessions
            .get_messages("session")
            .await
            .unwrap()
            .iter()
            .all(|m| !m.content.contains("cancelled"))
    );
}

#[test]
fn global_turn_limit_survives_new_widget_identities_and_queued_work() {
    let registry = Arc::new(Registry::default());
    let first = registry.admit("widget:1", "s", None, "1", 3, 2).unwrap();
    let queued = registry.admit("widget:2", "s", None, "2", 3, 2).unwrap();
    assert!(queued.predecessor.is_some());
    assert!(matches!(registry.admit("widget:3", "new-socket", None, "3", 3, 2), Err("turn_limit")));
    drop(first);
    assert!(registry.admit("widget:3", "new-socket", None, "3", 3, 2).is_ok());
}

#[test]
fn global_turn_config_fails_closed() {
    let home = tempfile::tempdir().unwrap();
    assert_eq!(configured_global_limit(home.path()), 10);
    for (value, expected) in [("1",1), ("64",64), ("65",0), ("0",0), ("true",0)] {
        std::fs::write(home.path().join("config.toml"), format!("[webchat]\nmax_global_turns={value}\n")).unwrap();
        assert_eq!(configured_global_limit(home.path()), expected);
    }
    std::fs::remove_file(home.path().join("config.toml")).unwrap();
    std::fs::create_dir(home.path().join("config.toml")).unwrap();
    assert_eq!(configured_global_limit(home.path()), 0);
}

#[tokio::test(start_paused = true)]
async fn stalled_socket_ack_and_writer_are_bounded() {
    let (tx, mut rx) = outbound_channel();
    let mut sink = TurnSink::new(tx, "s", None, "m").acknowledged();
    let task = tokio::spawn(async move { sink.send(Message::Text("{}".into())).await });
    let expired = rx.recv().await.unwrap();
    tokio::time::advance(SEND_TIMEOUT).await;
    assert!(task.await.unwrap().is_err());
    let sent = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let capture = sent.clone();
    let accepting = futures_util::sink::unfold(capture, |count, _: Message| async move {
        count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok::<_, ()>(count)
    });
    assert!(expired.deliver(&mut BoundedSocketSink(Box::pin(accepting))).await.is_ok());
    assert_eq!(sent.load(std::sync::atomic::Ordering::SeqCst), 0);
    let stalled = futures_util::sink::unfold((), |(), _: Message| async {
        std::future::pending::<Result<(), ()>>().await
    });
    let mut writer = BoundedSocketSink(Box::pin(stalled));
    assert!(writer.send(Message::Ping(vec![].into())).await.is_err());
}

#[tokio::test]
async fn panic_emits_correlated_terminal_frame_after_cleanup() {
    let (tx, mut rx) = outbound_channel();
    let sink = TurnSink::new(tx, "s", Some("conv".into()), "m");
    supervise(&sink, async {
        crate::channel_reply::with_delivery_cleanup(async { panic!("injected turn panic") }).await;
    }).await;
    let Message::Text(text) = rx.recv().await.unwrap().message else { panic!("text") };
    let frame: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(frame["code"], "internal_error");
    assert_eq!(frame["session_id"], "s");
    assert_eq!(frame["conv"], "conv");
    assert_eq!(frame["msg_id"], "m");
}

#[tokio::test]
async fn queued_cancel_keeps_fifo_and_never_creates_session() {
    let home = tempfile::tempdir().unwrap();
    let sessions = crate::session::SessionManager::new(&home.path().join("sessions.db")).unwrap();
    let registry = Arc::new(Registry::default());
    let first = registry.admit("alice", "s", None, "1", 3, 10).unwrap();
    let mut queued = registry.admit("alice", "s", None, "2", 3, 10).unwrap();
    registry.cancel("alice", Some("s"), None, Some("2"), None).unwrap();
    assert_eq!(queued.predecessor.as_mut().unwrap().try_recv(), Err(oneshot::error::TryRecvError::Empty));
    drop(first);
    queued.predecessor.take().unwrap().await.unwrap();
    let (cancelled, cleanup) = run_cancellable("s", &queued, async { panic!("queued model must not run") }).await;
    assert!(cancelled);
    let (tx, mut rx) = outbound_channel();
    finish_cancel(home.path(), None, &sessions, "s", "bot", &queued,
        &TurnSink::new(tx, "s", None, "2"), cleanup).await;
    assert!(sessions.session_agent("s").await.unwrap().is_none());
    let conn = rusqlite::Connection::open(home.path().join("approvals.db")).unwrap();
    let exists: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='cancelled_approval_turns')", [], |r| r.get(0)).unwrap();
    assert!(!exists, "never-started cancellation must not even open the approval store");
    let Message::Text(text) = rx.recv().await.unwrap().message else { panic!("text") };
    assert_eq!(serde_json::from_str::<serde_json::Value>(&text).unwrap()["code"], "cancelled");
}

#[test]
fn duplicate_at_capacity_is_never_a_terminal_limit_error() {
    let registry = Arc::new(Registry::default());
    let _active = registry.admit("alice", "s", None, "m", 1, 1).unwrap();
    assert!(matches!(registry.admit("alice", "s", None, "m", 1, 1), Err("duplicate_message")));
}

#[tokio::test]
async fn explicit_selection_rechecks_access_without_extra_resume_requirements() {
    let cancel = CancellationToken::new();
    assert!(recheck_turn_access(&cancel, false, async { true }, async {
        panic!("a plain explicit selection must not run the operational/may_call predicate")
    }).await);
    assert!(!recheck_turn_access(&cancel, false, async { false }, async { true }).await);
    assert!(!recheck_turn_access(&cancel, true, async { true }, async { false }).await);
}

#[tokio::test]
async fn cancellation_during_acl_recheck_records_cancelled_and_never_runs_model() {
    let home = tempfile::tempdir().unwrap();
    let sessions = crate::session::SessionManager::new(&home.path().join("sessions.db")).unwrap();
    sessions.get_or_create("session", "bot").await.unwrap();
    let registry = Arc::new(Registry::default());
    let ticket = registry.admit("alice", "session", None, "message", 3, 10).unwrap();
    let (started, waiting) = oneshot::channel();
    let recheck = recheck_turn_access(&ticket.cancel, true, async {
        started.send(()).unwrap();
        std::future::pending::<bool>().await
    }, async { false });
    let cancellation = async {
        waiting.await.unwrap();
        registry.cancel("alice", Some("session"), None, Some("message"), None).unwrap();
    };
    let (allowed, ()) = tokio::join!(recheck, cancellation);
    assert!(allowed, "cancellation must proceed to the terminal cleanup instead of ACL denial");
    let (cancelled, cleanup) = run_cancellable("session", &ticket, async {
        panic!("a turn cancelled during ACL recheck must not start the model")
    }).await;
    assert!(cancelled);
    let (tx, mut rx) = outbound_channel();
    finish_cancel(home.path(), Some(&crate::approval::ApprovalBroker::open(home.path()).unwrap()), &sessions, "session", "bot", &ticket,
        &TurnSink::new(tx, "session", Some("conversation".into()), "message"), cleanup).await;
    let Message::Text(text) = rx.recv().await.unwrap().message else { panic!("text") };
    let frame: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(frame["code"], "cancelled");
    assert_eq!(frame["session_id"], "session");
    assert_eq!(frame["msg_id"], "message");
    let recorded: String = sessions.acquire().await.query_row(
        "SELECT turn_id FROM webchat_turn_cancellations WHERE session_id='session'", [], |r| r.get(0),
    ).unwrap();
    assert_eq!(recorded, ticket.id);
}

#[tokio::test]
async fn persistent_cancel_quota_refuses_admission_but_inflight_cancel_still_fences() {
    let home = tempfile::tempdir().unwrap();
    let registry = Arc::new(Registry::default());
    let broker = crate::approval::ApprovalBroker::open(home.path()).unwrap();
    let ticket = registry.admit_persistent(Some(&broker), "user:alice", "alice", "session", None, "active", 3, 10)
        .await.unwrap();
    let broker = crate::approval::ApprovalBroker::open(home.path()).unwrap();
    let (started, cleanup) = run_cancellable("session", &ticket, async {
        crate::approval::scope_turn("session", &ticket.id,
            broker.request("bot", "test", "pending", serde_json::Value::Null, 600)).await.unwrap();
    }).await;
    assert!(!started);
    cleanup.unwrap();
    for n in 0..100 {
        broker.withdraw_turn_for_principal("history", &format!("cancel-{n}"), Some("user:alice")).await.unwrap();
    }
    drop(broker);
    let broker = crate::approval::ApprovalBroker::open(home.path()).unwrap();
    // A new registry and broker emulate restart: no in-memory quota survives.
    let restarted = Arc::new(Registry::default());
    assert!(matches!(restarted.admit_persistent(Some(&broker), "user:alice", "alice", "new", None, "next", 3, 10).await,
        Err("turn_limit")));
    assert!(restarted.entries.lock().unwrap().is_empty(), "quota refusal must release the slot");
    assert!(restarted.admit_persistent(Some(&broker), "user:bob", "bob", "other", None, "next", 3, 10).await.is_ok());
    assert!(matches!(registry.admit_persistent(Some(&broker), "user:alice", "alice", "session", None, "active", 3, 10).await,
        Err("duplicate_message")));
    registry.cancel("alice", Some("session"), None, Some("active"), None).unwrap();
    let sessions = crate::session::SessionManager::new(&home.path().join("sessions.db")).unwrap();
    let (tx, mut rx) = outbound_channel();
    finish_cancel(home.path(), Some(&crate::approval::ApprovalBroker::open(home.path()).unwrap()), &sessions, "session", "bot", &ticket,
        &TurnSink::new(tx, "session", None, "active"), Ok(())).await;
    let Message::Text(text) = rx.recv().await.unwrap().message else { panic!("text") };
    assert_eq!(serde_json::from_str::<serde_json::Value>(&text).unwrap()["code"], "cancelled");
    let broker = crate::approval::ApprovalBroker::open(home.path()).unwrap();
    assert!(crate::approval::scope_turn("session", &ticket.id,
        broker.request("bot", "test", "late", serde_json::Value::Null, 600)).await.is_err());
    let conn = rusqlite::Connection::open(home.path().join("approvals.db")).unwrap();
    let count: i64 = conn.query_row("SELECT COUNT(*) FROM cancelled_approval_turns WHERE principal='user:alice'", [], |r| r.get(0)).unwrap();
    assert_eq!(count, 101);
    conn.execute("UPDATE cancelled_approval_turns SET cancelled_at=datetime('now','-61 minutes')", []).unwrap();
    assert!(restarted.admit_persistent(Some(&broker), "user:alice", "alice", "new", None, "next", 3, 10).await.is_ok());
}

#[tokio::test]
async fn tombstone_row_cap_never_refuses_new_turns() {
    let home = tempfile::tempdir().unwrap();
    let broker = crate::approval::ApprovalBroker::open(home.path()).unwrap();
    let conn = rusqlite::Connection::open(home.path().join("approvals.db")).unwrap();
    conn.execute("WITH RECURSIVE ids(n) AS (SELECT 1 UNION ALL SELECT n+1 FROM ids WHERE n<100000)
        INSERT INTO cancelled_approval_turns(session_id,turn_id,cancelled_at)
        SELECT 'old',CAST(n AS TEXT),datetime('now') FROM ids", []).unwrap();
    let registry = Arc::new(Registry::default());
    assert!(registry.admit_persistent(Some(&broker), "user:alice", "alice", "s", None, "new", 3, 10).await.is_ok());
    broker.withdraw_turn_for_principal("s", "inflight", Some("user:alice")).await.unwrap();
    let count: i64 = conn.query_row("SELECT COUNT(*) FROM cancelled_approval_turns", [], |r| r.get(0)).unwrap();
    assert_eq!(count, 100001);
}

#[test]
fn cancel_quota_principal_uses_account_or_transport_ip() {
    let ip = "192.0.2.1".parse().unwrap();
    assert_eq!(cancel_principal("widget-visitor:first", ip), "ip:192.0.2.1");
    assert_eq!(cancel_principal("widget-visitor:second", ip), "ip:192.0.2.1");
    assert_eq!(cancel_principal("alice", ip), "user:alice");
    assert_eq!(cancel_principal("alice", "192.0.2.2".parse().unwrap()), "user:alice");
}


#[tokio::test]
async fn over_memory_caps_never_reads_persistent_quota() {
    let home = tempfile::tempdir().unwrap();
    let broker = crate::approval::ApprovalBroker::open(home.path()).unwrap();
    let registry = Arc::new(Registry::default());
    let _active = registry.admit_persistent(Some(&broker), "user:alice", "alice", "s", None, "active", 1, 2).await.unwrap();
    assert_eq!(registry.quota_reads.load(Ordering::Relaxed), 1);
    for n in 0..1000 {
        assert!(matches!(registry.admit_persistent(Some(&broker), "user:alice", "alice", "s", None, &n.to_string(), 1, 2).await, Err("turn_limit")));
        assert!(matches!(registry.admit_persistent(Some(&broker), "user:bob", "bob", "b", None, &n.to_string(), 1, 1).await, Err("turn_limit")));
    }
    assert_eq!(registry.quota_reads.load(Ordering::Relaxed), 1);
}

#[tokio::test]
async fn refused_reservation_releases_slot_without_breaking_fifo() {
    let registry = Arc::new(Registry::default());
    let first = registry.admit("alice", "s", None, "first", 3, 10).unwrap();
    let rejected = registry.reserve("alice", "s", None, "rejected", 3, 10).unwrap();
    let mut last = registry.admit("alice", "s", None, "last", 3, 10).unwrap();
    drop(rejected);
    assert_eq!(registry.entries.lock().unwrap().len(), 2);
    tokio::task::yield_now().await;
    assert_eq!(last.predecessor.as_mut().unwrap().try_recv(), Err(oneshot::error::TryRecvError::Empty));
    drop(first);
    last.predecessor.take().unwrap().await.unwrap();
}

#[tokio::test]
async fn quota_waiting_reservation_cannot_be_cancelled_until_joining_fifo() {
    let home = tempfile::tempdir().unwrap();
    let broker = crate::approval::ApprovalBroker::open(home.path()).unwrap();
    // Exercise the reserved state held throughout the quota await, without
    // relying on blocking-pool scheduling or a sleep to expose the race.
    let registry = Arc::new(Registry::default());
    let mut ticket = registry.reserve("alice", "s", Some("c".into()), "m", 3, 10).unwrap();
    for (sid, conv, msg) in [(Some("s"), None, Some("m")), (None, Some("c"), Some("m")), (Some("s"), None, None)] {
        assert!(registry.cancel("alice", sid, conv, msg, None).is_err());
    }
    assert!(!ticket.cancel.is_cancelled());
    broker.check_webchat_turn_admission("user:alice").await.unwrap();
    ticket.join_lane("s");
    assert!(registry.cancel("alice", Some("s"), None, Some("m"), None).is_ok());
}

#[tokio::test]
async fn delegation_queue_failure_reports_cancel_failed_after_fence_commits() {
    let home = tempfile::tempdir().unwrap();
    let broker = crate::approval::ApprovalBroker::open(home.path()).unwrap();
    let sessions = crate::session::SessionManager::new(&home.path().join("sessions.db")).unwrap();
    std::fs::create_dir(home.path().join("message_queue.db")).unwrap();
    let registry = Arc::new(Registry::default());
    let ticket = registry.admit("alice", "s", None, "m", 3, 10).unwrap();
    ticket.started.store(true, Ordering::Release);
    let (tx, mut rx) = outbound_channel();
    finish_cancel(home.path(), Some(&broker), &sessions, "s", "bot", &ticket,
        &TurnSink::new(tx, "s", None, "m"), Ok(())).await;
    let Message::Text(text) = rx.recv().await.unwrap().message else { panic!("expected text"); };
    assert_eq!(serde_json::from_str::<serde_json::Value>(&text).unwrap()["code"], "cancel_failed");
    assert!(crate::approval::is_turn_cancelled(home.path(), "s", &ticket.id).await.unwrap());
}

#[tokio::test]
async fn revoked_or_unavailable_identity_withholds_queued_frames_and_rejects_ack() {
    use crate::handlers::live_context::{SocketRecovery, SocketValidity};
    for validity in [SocketValidity::Revoked, SocketValidity::Unavailable] {
        let sent = Arc::new(std::sync::Mutex::new(Vec::new()));
        let captured = sent.clone();
        let accepting = futures_util::sink::unfold(captured, |captured, message: Message| async {
            captured.lock().unwrap().push(message);
            Ok::<_, ()>(captured)
        });
        let mut writer = BoundedSocketSink(Box::pin(accepting));
        let (ack, received) = oneshot::channel();
        let frame = Outbound { message: Message::Text("sensitive".into()), ack: Some(ack),
            deadline: Some(tokio::time::Instant::now() + SEND_TIMEOUT) };
        let mut recovery = SocketRecovery::default();
        let result = frame.deliver_authorized(validity, &mut recovery, &mut writer).await;
        assert!(!received.await.unwrap());
        assert!(sent.lock().unwrap().iter().all(|message| matches!(message, Message::Close(_))));
        assert_eq!(recovery.closed(), validity == SocketValidity::Revoked);
        assert_eq!(result.is_err(), validity == SocketValidity::Revoked);
        if validity == SocketValidity::Unavailable {
            Outbound { message: Message::Text("recovered".into()), ack: None, deadline: None }
                .deliver_authorized(SocketValidity::Valid, &mut recovery, &mut writer).await.unwrap();
            assert!(matches!(&sent.lock().unwrap()[0], Message::Text(text) if text.as_str() == "recovered"));
        }
    }
}
