use super::*;
fn event(id: &str, conversation: &str) -> AcceptedEvent {
    AcceptedEvent {
        decision_fastlane: false,
        decision_binding: None,
        event_id: id.into(),
        account: "account".into(),
        revision: "r1".into(),
        authorization_revision: "auth1".into(),
        conversation: conversation.into(),
        payload: format!("{{\"replyToken\":\"SECRET-{id}\"}}"),
    }
}

#[tokio::test]
async fn append_is_atomic_deduplicated_and_private() {
    let dir = tempfile::tempdir().unwrap();
    let store = IngressStore::open(dir.path()).unwrap();
    store
        .append(&[event("a", "chat"), event("a", "chat")], 10)
        .await
        .unwrap();
    let rows = store.list().await.unwrap();
    assert_eq!(rows.len(), 1);
    let serialized = serde_json::to_string(&rows).unwrap();
    assert!(!serialized.contains("SECRET"));
    assert!(!serialized.contains("replyToken"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(dir.path().join("channel_ingress.db"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
}

#[tokio::test]
async fn acknowledged_append_survives_reopen_and_orders_conversations() {
    let dir = tempfile::tempdir().unwrap();
    {
        let s = IngressStore::open(dir.path()).unwrap();
        s.append(
            &[event("a", "chat"), event("b", "chat"), event("c", "other")],
            10,
        )
        .await
        .unwrap();
    }
    let s = IngressStore::open(dir.path()).unwrap();
    let first = s.claim(11).await.unwrap().unwrap();
    assert_eq!(first.event_id, "a");
    let other = s.claim(11).await.unwrap().unwrap();
    assert_eq!(other.event_id, "c");
    assert!(s.claim(11).await.unwrap().is_none());
    assert!(
        s.transition(&first, "claimed", "dispatching", None)
            .await
            .unwrap()
    );
    assert!(
        s.transition(&first, "dispatching", "completed", None)
            .await
            .unwrap()
    );
    assert_eq!(s.claim(12).await.unwrap().unwrap().event_id, "b");
}

#[tokio::test]
async fn pre_dispatch_crash_retries_but_dispatch_crash_requires_review() {
    let dir = tempfile::tempdir().unwrap();
    let s = IngressStore::open(dir.path()).unwrap();
    s.append(&[event("a", "chat"), event("b", "chat")], 10)
        .await
        .unwrap();
    let first = s.claim(10).await.unwrap().unwrap();
    s.recover_and_purge(101).await.unwrap();
    let second = s.claim(102).await.unwrap().unwrap();
    assert_eq!(first.id, second.id);
    assert_eq!(second.attempt, 2);
    assert!(
        !s.transition(&first, "claimed", "dispatching", None)
            .await
            .unwrap()
    );
    assert!(
        s.transition(&second, "claimed", "dispatching", None)
            .await
            .unwrap()
    );
    s.recover_and_purge(193).await.unwrap();
    assert!(s.claim(194).await.unwrap().is_none());
    let rows = s.list().await.unwrap();
    let row = rows.iter().find(|r| r.id == first.id).unwrap();
    assert_eq!(row.status, "uncertain");
    assert_eq!(row.reason.as_deref(), Some("dispatch_receipt_missing"));
    assert!(
        s.resolve(
            &row.id,
            "stale",
            "close",
            false,
            "operator",
            "checked",
            194,
            row.attempt
        )
        .await
        .is_err()
    );
    assert!(
        s.resolve(
            &row.id,
            "r1",
            "retry",
            false,
            "operator",
            "checked",
            194,
            row.attempt
        )
        .await
        .is_err()
    );
    s.resolve(
        &row.id,
        "r1",
        "close",
        false,
        "operator",
        "provider confirmed receipt",
        194,
        row.attempt,
    )
    .await
    .unwrap();
    assert_eq!(s.claim(195).await.unwrap().unwrap().event_id, "b");
}

#[tokio::test]
async fn payload_expires_but_tombstone_and_blocked_backlog_survive() {
    let dir = tempfile::tempdir().unwrap();
    let s = IngressStore::open(dir.path()).unwrap();
    s.append(&[event("a", "chat")], 10).await.unwrap();
    s.recover_and_purge(10 + PAYLOAD_SECONDS).await.unwrap();
    let row = s.list().await.unwrap().remove(0);
    assert!(row.payload.is_none());
    assert_eq!(row.status, "quarantined");
    s.append(&[event("a", "chat")], 20 + PAYLOAD_SECONDS)
        .await
        .unwrap();
    assert_eq!(s.list().await.unwrap().len(), 1);
    assert!(s.list().await.unwrap()[0].payload.is_none());
    assert!(
        s.resolve(
            &row.id,
            "r1",
            "retry",
            true,
            "operator",
            "checked",
            90000,
            row.attempt
        )
        .await
        .is_err()
    );
    s.resolve(
        &row.id,
        "r1",
        "close",
        false,
        "operator",
        "expired",
        90000,
        row.attempt,
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn concurrent_workers_only_one_claim_and_account_routes_isolated() {
    let dir = tempfile::tempdir().unwrap();
    let a = IngressStore::open(dir.path()).unwrap();
    let b = IngressStore::open(dir.path()).unwrap();
    a.append(&[event("a", "chat")], 10).await.unwrap();
    let (one, two) = tokio::join!(a.claim(11), b.claim(11));
    assert_eq!(
        one.unwrap().is_some() as u8 + two.unwrap().is_some() as u8,
        1
    );
    let mut route = event("a", "chat");
    route.revision = "r2".into();
    a.append(&[route], 12).await.unwrap();
    let mut account = event("a", "chat");
    account.account = "other-account".into();
    a.append(&[account], 12).await.unwrap();
    assert_eq!(a.list().await.unwrap().len(), 2);
}

#[tokio::test]
async fn sqlite_full_rolls_back_whole_envelope() {
    let dir = tempfile::tempdir().unwrap();
    let s = IngressStore::open(dir.path()).unwrap();
    {
        let conn = s.conn.lock().await;
        conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);
            PRAGMA max_page_count=8;")
            .unwrap();
    }
    let mut large = event("large", "chat");
    large.payload = "x".repeat(1024 * 1024);
    assert!(s.append(&[event("a", "chat"), large], 10).await.is_err());
    assert!(s.list().await.unwrap().is_empty());
}

#[tokio::test]
async fn authorization_change_cannot_reinsert_same_event() {
    let dir = tempfile::tempdir().unwrap();
    let s = IngressStore::open(dir.path()).unwrap();
    s.append(&[event("a", "chat")], 10).await.unwrap();
    let mut changed = event("a", "chat");
    changed.authorization_revision = "new-policy".into();
    changed.payload = "replacement-token".into();
    s.append(&[changed], 11).await.unwrap();
    let rows = s.list().await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].authorization_revision, "auth1");
    assert!(!rows[0].payload.as_ref().unwrap().contains("replacement"));
}

#[tokio::test]
async fn operator_retry_is_explicit_and_cas_audited() {
    let dir = tempfile::tempdir().unwrap();
    let s = IngressStore::open(dir.path()).unwrap();
    s.append(&[event("a", "chat")], 10).await.unwrap();
    let row = s.claim(10).await.unwrap().unwrap();
    s.transition(&row, "claimed", "dispatching", None)
        .await
        .unwrap();
    s.transition(&row, "dispatching", "uncertain", Some("missing_receipt"))
        .await
        .unwrap();
    assert!(
        s.resolve(
            &row.id,
            "r1",
            "rerun",
            true,
            "",
            "reviewed",
            11,
            row.attempt
        )
        .await
        .is_err()
    );
    assert!(
        s.resolve(
            &row.id,
            "r1",
            "rerun",
            true,
            "operator",
            "",
            11,
            row.attempt
        )
        .await
        .is_err()
    );
    s.resolve(
        &row.id,
        "r1",
        "rerun",
        true,
        "operator",
        "verified safe to retry",
        11,
        row.attempt,
    )
    .await
    .unwrap();
    assert!(
        s.resolve(
            &row.id,
            "r1",
            "rerun",
            true,
            "operator",
            "race",
            11,
            row.attempt
        )
        .await
        .is_err()
    );
    let conn = s.conn.lock().await;
    assert_eq!(
        conn.query_row("SELECT count(*) FROM ingress_resolutions", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        1
    );
}

/// Re-entered by the parent test and killed with the OS API at the actual boundary.
#[tokio::test]
async fn crash_fixture_process() {
    let Ok(root) = std::env::var("DUDU_TEST_INGRESS_CRASH_ROOT") else {
        return;
    };
    let point = std::env::var("DUDU_TEST_INGRESS_CRASH_POINT").unwrap();
    let home = std::path::Path::new(&root);
    let s = IngressStore::open(home).unwrap();
    if point == "before_commit" {
        let mut conn = s.conn.lock().await;
        let tx = conn.transaction().unwrap();
        tx.execute(
            "INSERT INTO ingress(id,channel,account,event_id,revision,authorization_revision,conversation,received_at,
                run_id) VALUES ('uncommitted','line','account','a','r1','auth1','chat',10,'uncommitted')",
            []
        )
        .unwrap();
        std::fs::write(home.join("kill-ready"), "ready").unwrap();
        std::thread::sleep(std::time::Duration::from_secs(30));
        drop(tx);
    } else {
        s.append(&[event("a", "chat")], 10).await.unwrap();
        if point == "dispatching" {
            let row = s.claim(10).await.unwrap().unwrap();
            s.transition(&row, "claimed", "dispatching", None)
                .await
                .unwrap();
        }
        std::fs::write(home.join("kill-ready"), "ready").unwrap();
        std::thread::sleep(std::time::Duration::from_secs(30));
    }
}

#[tokio::test]
async fn actual_process_kill_preserves_ack_and_never_repeats_unknown_dispatch() {
    for point in ["before_commit", "after_commit", "dispatching"] {
        let dir = tempfile::tempdir().unwrap();
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "channel_ingress::tests::crash_fixture_process",
                "--test-threads=1",
            ])
            .env("DUDU_TEST_INGRESS_CRASH_ROOT", dir.path())
            .env("DUDU_TEST_INGRESS_CRASH_POINT", point)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        for _ in 0..500 {
            if dir.path().join("kill-ready").exists() {
                break;
            }
            if let Some(status) = child.try_wait().unwrap() {
                panic!("crash child exited early: {status}");
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        let ready = dir.path().join("kill-ready").exists();
        child.kill().unwrap();
        child.wait().unwrap();
        assert!(ready, "crash fixture did not reach {point}");
        let s = IngressStore::open(dir.path()).unwrap();
        s.recover_and_purge(101).await.unwrap();
        let rows = s.list().await.unwrap();
        match point {
            "before_commit" => assert!(rows.is_empty()),
            "after_commit" => {
                assert_eq!(rows.len(), 1);
                assert_eq!(s.claim(102).await.unwrap().unwrap().event_id, "a");
            }
            _ => {
                assert_eq!(rows[0].status, "uncertain");
                assert!(s.claim(102).await.unwrap().is_none());
            }
        }
    }
}

#[tokio::test]
async fn uncertain_retry_retains_immutable_attempt_and_new_authorization_lineage() {
    let dir = tempfile::tempdir().unwrap();
    let s = IngressStore::open(dir.path()).unwrap();
    s.append(&[event("a", "chat")], 10).await.unwrap();
    let old = s.claim(10).await.unwrap().unwrap();
    assert_eq!(old.run_id, old.id);
    assert!(old.run_authorization_id.is_none());
    s.transition(&old, "claimed", "dispatching", None)
        .await
        .unwrap();
    s.transition(
        &old,
        "dispatching",
        "uncertain",
        Some("provider_receipt_missing"),
    )
    .await
    .unwrap();
    s.resolve(
        &old.id,
        "r1",
        "rerun",
        true,
        "operator",
        "accept known duplicate risk; repeat this action",
        11,
        old.attempt,
    )
    .await
    .unwrap();
    let retry = s.claim(12).await.unwrap().unwrap();
    assert_ne!(old.run_id, retry.run_id);
    assert_eq!(
        retry.run_authorization_id.as_deref(),
        Some(retry.run_id.as_str())
    );
    assert_ne!(old.lease_id, retry.lease_id);
    s.transition(&retry, "claimed", "dispatching", None)
        .await
        .unwrap();
    s.transition(&retry, "dispatching", "completed", None)
        .await
        .unwrap();
    let summary = s.summary().await.unwrap();
    let attempts = summary["attempts"].as_array().unwrap();
    assert_eq!(attempts.len(), 2);
    let predecessor = attempts
        .iter()
        .find(|r| r["status"] == "uncertain")
        .unwrap();
    assert_eq!(
        predecessor["operation_id"].as_str(),
        old.lease_id.as_deref()
    );
    assert_eq!(predecessor["reason"], "provider_receipt_missing");
    let new = attempts
        .iter()
        .find(|r| r["status"] == "completed")
        .unwrap();
    assert!(new["retry_authorization_id"].is_string());
    assert_eq!(
        summary["retry_authorizations"][0]["predecessor_operation_id"],
        predecessor["operation_id"]
    );
    let conn = s.conn.lock().await;
    assert!(
        conn.execute("UPDATE ingress_attempts SET status='completed'", [])
            .is_err()
    );
    assert!(conn.execute("DELETE FROM ingress_attempts", []).is_err());
}

#[tokio::test]
async fn two_explicit_retries_have_atomic_distinct_runs_and_old_workers_cannot_finish_new_run() {
    let dir = tempfile::tempdir().unwrap();
    let store = IngressStore::open(dir.path()).unwrap();
    store
        .append(&[event("run-lineage", "chat")], 10)
        .await
        .unwrap();
    let first = store.claim(10).await.unwrap().unwrap();
    assert_eq!(first.run_id, first.id);
    assert!(
        store
            .transition(&first, "claimed", "dispatching", None)
            .await
            .unwrap()
    );
    assert!(
        store
            .transition(&first, "dispatching", "uncertain", Some("missing"))
            .await
            .unwrap()
    );
    store
        .resolve(
            &first.id,
            "r1",
            "rerun",
            true,
            "operator",
            "first retry",
            100,
            first.attempt,
        )
        .await
        .unwrap();
    let second = store.claim(101).await.unwrap().unwrap();
    assert_ne!(first.run_id, second.run_id);
    assert!(
        store
            .transition(&second, "claimed", "dispatching", None)
            .await
            .unwrap()
    );
    assert!(
        store
            .transition(&second, "dispatching", "uncertain", Some("missing again"))
            .await
            .unwrap()
    );
    // Time is deliberately out of order. Active authorization is a stored FK,
    // not whichever record happens to have the largest wall-clock timestamp.
    store
        .resolve(
            &first.id,
            "r1",
            "rerun",
            true,
            "operator",
            "second retry",
            99,
            second.attempt,
        )
        .await
        .unwrap();
    let third = store.claim(102).await.unwrap().unwrap();
    assert_eq!(first.id, third.id);
    assert_ne!(second.run_id, third.run_id);
    assert_eq!(
        third.run_authorization_id.as_deref(),
        Some(third.run_id.as_str())
    );
    assert!(
        store
            .transition(&third, "claimed", "dispatching", None)
            .await
            .unwrap()
    );
    for stale in [&first, &second] {
        assert!(!store.renew(stale, 103).await.unwrap());
        assert!(
            !store
                .transition(stale, "dispatching", "completed", None)
                .await
                .unwrap()
        );
    }
    assert!(
        store
            .transition(&third, "dispatching", "completed", None)
            .await
            .unwrap()
    );
    let inspected = store.inspect(&first.id, None, None).await.unwrap().unwrap();
    assert_eq!(inspected["event"]["run_id"], third.run_id);
    assert_eq!(
        inspected["attempts"][0]["retry_authorization_id"],
        third.run_id
    );
    assert_eq!(inspected["attempts"].as_array().unwrap().len(), 3);
    let reopened = IngressStore::open(dir.path()).unwrap();
    assert_eq!(reopened.list().await.unwrap()[0].run_id, third.run_id);
}
