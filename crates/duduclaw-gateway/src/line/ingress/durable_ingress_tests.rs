use super::*;
use crate::protocol::WsFrame;
use base64::Engine;

async fn fixture() -> (tempfile::TempDir, LineState) {
    let dir = tempfile::tempdir().unwrap();
    let handler = crate::handlers::MethodHandler::new(dir.path().to_path_buf()).await;
    let frame = handler
        .handle(
            "agents.create",
            serde_json::json!({"name":"line-agent","display_name":"LINE"}),
            &duduclaw_auth::UserContext::admin_fallback(),
        )
        .await;
    assert!(
        matches!(frame, WsFrame::Response { ok: true, .. }),
        "agent fixture failed: {frame:?}"
    );
    std::fs::write(
        dir.path().join("config.toml"),
        "[channels]\nline_channel_token='test-access'\nline_channel_secret='test-secret'\n",
    )
    .unwrap();
    let mut registry = duduclaw_agent::AgentRegistry::new(dir.path().join("agents"));
    registry.scan().await.unwrap();
    // AgentResolver falls back to the main role; a channel permission selects this fixture.
    let path = dir.path().join("agents/line-agent/agent.toml");
    let mut cfg: toml::Table = std::fs::read_to_string(&path).unwrap().parse().unwrap();
    cfg.get_mut("permissions")
        .unwrap()
        .as_table_mut()
        .unwrap()
        .insert(
            "allowed_channels".into(),
            toml::Value::Array(vec![toml::Value::String("line".into())]),
        );
    std::fs::write(path, toml::to_string(&cfg).unwrap()).unwrap();
    registry.scan().await.unwrap();
    let sessions =
        Arc::new(crate::session::SessionManager::new(&dir.path().join("sessions.db")).unwrap());
    let (event_tx, _) = tokio::sync::broadcast::channel(16);
    let channel_status = Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new()));
    let ctx = Arc::new(ReplyContext::new(
        Arc::new(tokio::sync::RwLock::new(registry)),
        dir.path().to_path_buf(),
        sessions,
        channel_status.clone(),
        event_tx.clone(),
    ));
    // No drain until the test explicitly starts the recovery hook.
    let state = LineState {
        home_dir: dir.path().to_path_buf(),
        ctx,
        http: reqwest::Client::new(),
        channel_status,
        event_tx,
        ingress: Some(Arc::new(
            crate::channel_ingress::IngressStore::open(dir.path()).unwrap(),
        )),
    };
    (dir, state)
}
fn envelope(id: &str) -> serde_json::Value {
    serde_json::json!({
        "destination": "bot-account",
        "events": [
                      {
                          "type": "follow",
                          "webhookEventId": id,
                          "replyToken": "sensitive-reply-token",
                          "source": {"type":"user","userId":"Utest"}
                      }
                  ]
    })
}
fn signed(value: &serde_json::Value) -> (HeaderMap, Bytes) {
    let body = serde_json::to_vec(value).unwrap();
    let mut mac = HmacSha256::new_from_slice(b"test-secret").unwrap();
    mac.update(&body);
    let mut headers = HeaderMap::new();
    headers.insert(
        "x-line-signature",
        base64::engine::general_purpose::STANDARD
            .encode(mac.finalize().into_bytes())
            .parse()
            .unwrap(),
    );
    (headers, Bytes::from(body))
}
#[tokio::test]
async fn durable_ack_precedes_recovery_and_redelivery_is_one_event() {
    let (_dir, state) = fixture().await;
    let (headers, body) = signed(&envelope("event-1"));
    assert_eq!(
        handle_line_webhook(state.clone(), &headers, body.clone()).await,
        StatusCode::OK
    );
    assert_eq!(
        handle_line_webhook(state.clone(), &headers, body).await,
        StatusCode::OK
    );
    let rows = state.ingress.as_ref().unwrap().list().await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].status, "ready");
    let worker = tokio::spawn(drain_line_ingress(state.clone()));
    for _ in 0..100 {
        if state.ingress.as_ref().unwrap().list().await.unwrap()[0].status == "completed" {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    worker.abort();
    assert_eq!(
        state.ingress.as_ref().unwrap().list().await.unwrap()[0].status,
        "completed"
    );
}
#[tokio::test]
async fn failed_signature_missing_identity_and_disk_full_never_ack() {
    let (_dir, state) = fixture().await;
    let (headers, body) = signed(&envelope("event-1"));
    assert_eq!(
        handle_line_webhook(state.clone(), &HeaderMap::new(), body.clone()).await,
        StatusCode::BAD_REQUEST
    );
    let mut invalid = headers.clone();
    invalid.insert("x-line-signature", "bad".parse().unwrap());
    assert_eq!(
        handle_line_webhook(state.clone(), &invalid, body).await,
        StatusCode::UNAUTHORIZED
    );
    let mut missing = envelope("event-1");
    missing["events"][0]
        .as_object_mut()
        .unwrap()
        .remove("webhookEventId");
    let (h, b) = signed(&missing);
    assert_eq!(
        handle_line_webhook(state.clone(), &h, b).await,
        StatusCode::BAD_REQUEST
    );
    state.ingress.as_ref().unwrap().simulate_full().await;
    let mut huge = envelope("event-1");
    huge["events"][0]["extra"] = "x".repeat(1024 * 1024).into();
    let (h, b) = signed(&huge);
    assert_eq!(
        handle_line_webhook(state.clone(), &h, b).await,
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert!(
        state
            .ingress
            .as_ref()
            .unwrap()
            .list()
            .await
            .unwrap()
            .is_empty()
    );
}
#[tokio::test]
async fn credential_rotation_and_policy_change_quarantine_backlog_without_replay() {
    let (dir, state) = fixture().await;
    let (h, b) = signed(&envelope("event-1"));
    assert_eq!(
        handle_line_webhook(state.clone(), &h, b.clone()).await,
        StatusCode::OK
    );
    std::fs::write(
        dir.path().join("config.toml"),
        "[channels]\nline_channel_token='rotated-token'\nline_channel_secret='test-secret'\n",
    )
    .unwrap();
    assert_eq!(
        handle_line_webhook(state.clone(), &h, b).await,
        StatusCode::OK
    );
    assert_eq!(
        state.ingress.as_ref().unwrap().list().await.unwrap().len(),
        1
    );
    let worker = tokio::spawn(drain_line_ingress(state.clone()));
    for _ in 0..100 {
        if state.ingress.as_ref().unwrap().list().await.unwrap()[0].status == "quarantined" {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    worker.abort();
    let rows = state.ingress.as_ref().unwrap().list().await.unwrap();
    assert_eq!(rows[0].status, "quarantined");
    assert_eq!(
        rows[0].reason.as_deref(),
        Some("account_route_authorization_changed")
    );
}
#[tokio::test]
async fn rollback_pauses_backlog_and_refuses_new_ack() {
    let (dir, state) = fixture().await;
    let (h, b) = signed(&envelope("event-1"));
    assert_eq!(
        handle_line_webhook(state.clone(), &h, b.clone()).await,
        StatusCode::OK
    );
    std::fs::write(
        dir.path().join("config.toml"),
        "[channels]\nline_channel_token='test-access'\nline_channel_secret='test-secret'\n[channel_ingress]\nline_enabled=false\n"
    )
    .unwrap();
    assert_eq!(
        handle_line_webhook(state.clone(), &h, b).await,
        StatusCode::SERVICE_UNAVAILABLE
    );
    let worker = tokio::spawn(drain_line_ingress(state.clone()));
    tokio::time::sleep(std::time::Duration::from_millis(350)).await;
    worker.abort();
    assert_eq!(
        state.ingress.as_ref().unwrap().list().await.unwrap()[0].status,
        "ready"
    );
}
#[tokio::test]
async fn slow_conversation_does_not_block_others_and_same_chat_stays_ordered() {
    let (_dir, state) = fixture().await;
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    dispatch_test_hooks().lock().unwrap().insert(
        (state.home_dir.clone(), "slow".into()),
        (entered.clone(), release.clone()),
    );
    let mut slow = envelope("slow");
    slow["events"][0]["source"]["userId"] = "chat-A".into();
    let mut later = envelope("later");
    later["events"][0]["source"]["userId"] = "chat-A".into();
    let mut other = envelope("other");
    other["events"][0]["source"]["userId"] = "chat-B".into();
    for payload in [slow, later, other] {
        let (h, b) = signed(&payload);
        assert_eq!(
            handle_line_webhook(state.clone(), &h, b).await,
            StatusCode::OK
        );
    }
    let worker = tokio::spawn(drain_line_ingress(state.clone()));
    tokio::time::timeout(std::time::Duration::from_secs(3), entered.notified())
        .await
        .unwrap();
    for _ in 0..150 {
        let rows = state.ingress.as_ref().unwrap().list().await.unwrap();
        if rows
            .iter()
            .any(|r| r.event_id == "other" && r.status == "completed")
        {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    let rows = state.ingress.as_ref().unwrap().list().await.unwrap();
    assert_eq!(
        rows.iter().find(|r| r.event_id == "other").unwrap().status,
        "completed"
    );
    assert_eq!(
        rows.iter().find(|r| r.event_id == "later").unwrap().status,
        "ready"
    );
    release.notify_one();
    for _ in 0..150 {
        if state
            .ingress
            .as_ref()
            .unwrap()
            .list()
            .await
            .unwrap()
            .iter()
            .all(|r| r.status == "completed")
        {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    worker.abort();
    dispatch_test_hooks()
        .lock()
        .unwrap()
        .remove(&(state.home_dir.clone(), "slow".into()));
    assert!(
        state
            .ingress
            .as_ref()
            .unwrap()
            .list()
            .await
            .unwrap()
            .iter()
            .all(|r| r.status == "completed")
    );
}

#[tokio::test]
async fn authority_snapshot_refreshes_external_sql_writes_and_ignores_prompt_changes() {
    let (_dir, state) = fixture().await;
    let raw = envelope("event-1");
    let event = serde_json::from_value::<LineEvent>(raw["events"][0].clone()).unwrap();
    let first = line_revision(&state, &event, None).await.unwrap();
    let (route, auth) = (first.route, first.authority);
    // Populate the old cache, then mutate using another process-style SQLite connection.
    assert!(
        state
            .ctx
            .channel_settings
            .get("line", "global", keys::BLOCKED_USERS)
            .await
            .is_none()
    );
    let conn = rusqlite::Connection::open(state.home_dir.join("sessions.db")).unwrap();
    conn.execute(
        "INSERT INTO channel_settings VALUES ('line','global','blocked_users','[\"Utest\"]','changed')",
        []
    )
    .unwrap();
    let changed = line_revision(&state, &event, None).await.unwrap();
    let (new_route, new_auth) = (changed.route, changed.authority);
    assert_eq!(route, new_route);
    assert_ne!(auth, new_auth);
    assert_eq!(
        state
            .ctx
            .channel_settings
            .get("line", "global", keys::BLOCKED_USERS)
            .await
            .as_deref(),
        Some("[\"Utest\"]")
    );
    let path = state.home_dir.join("agents/line-agent/SOUL.md");
    std::fs::write(path, "new wording").unwrap();
    assert_eq!(
        line_revision(&state, &event, None).await.unwrap().authority,
        new_auth
    );
    conn.execute_batch("DROP TABLE channel_settings;").unwrap();
    assert!(line_revision(&state, &event, None).await.is_err());
}

async fn assert_worker_exit_stops_renewal(panic_worker: bool) {
    let (_dir, state) = fixture().await;
    let probe = Arc::new(WorkerTestProbe::default());
    worker_test_probes()
        .lock()
        .unwrap()
        .insert(state.home_dir.clone(), probe.clone());
    let event_id = if panic_worker {
        "panic-worker"
    } else {
        "aborted-worker"
    };
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    dispatch_test_hooks().lock().unwrap().insert(
        (state.home_dir.clone(), event_id.into()),
        (entered.clone(), release.clone()),
    );
    if panic_worker {
        probe.panic_events.lock().unwrap().insert(event_id.into());
    }
    for id in [event_id, "after-owner-exit"] {
        let (h, b) = signed(&envelope(id));
        assert_eq!(
            handle_line_webhook(state.clone(), &h, b).await,
            StatusCode::OK
        );
    }
    let worker_state = state.clone();
    let worker = tokio::spawn(async move {
        if panic_worker {
            drain_line_ingress(worker_state).await;
        } else {
            drain_line_worker(worker_state, IngressLane::Normal).await;
        }
    });
    tokio::time::timeout(std::time::Duration::from_secs(3), entered.notified())
        .await
        .unwrap();
    let store = state.ingress.as_ref().unwrap();
    let stale = store
        .list()
        .await
        .unwrap()
        .into_iter()
        .find(|r| r.event_id == event_id)
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if probe
                .renewals
                .lock()
                .unwrap()
                .get(&stale.id)
                .copied()
                .unwrap_or(0)
                >= 2
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    if panic_worker {
        release.notify_one();
        // Every dispatch lane counted one start; a restart adds one more.
        let lanes = crate::channel_ingress::config::IngressConfig::load(&state.home_dir)
            .await
            .line_workers
            + 1;
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while probe.starts.load(std::sync::atomic::Ordering::SeqCst) <= lanes {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
    } else {
        worker.abort();
    }
    let renewals = probe.renewals.lock().unwrap()[&stale.id];
    tokio::time::sleep(std::time::Duration::from_millis(120)).await;
    assert_eq!(
        probe.renewals.lock().unwrap()[&stale.id],
        renewals,
        "exited owner must not keep renewing"
    );
    let expired = chrono::Utc::now().timestamp() + crate::channel_ingress::LEASE_SECONDS + 5;
    store.recover_and_purge(expired).await.unwrap();
    let rows = store.list().await.unwrap();
    assert_eq!(
        rows.iter().find(|r| r.id == stale.id).unwrap().status,
        "uncertain"
    );
    assert_eq!(
        rows.iter()
            .find(|r| r.event_id == "after-owner-exit")
            .unwrap()
            .status,
        "ready"
    );
    assert!(
        store.claim(expired).await.unwrap().is_none(),
        "uncertain predecessor blocks automatic replay"
    );
    assert!(!store.renew(&stale, expired).await.unwrap());
    assert!(
        !store
            .transition(&stale, "dispatching", "completed", None)
            .await
            .unwrap()
    );
    let summary = store.summary().await.unwrap();
    assert!(
        summary["attempts"]
            .as_array()
            .unwrap()
            .iter()
            .any(|a| a["ingress_id"] == stale.id && a["status"] == "uncertain")
    );
    store
        .recover_and_purge(expired + crate::channel_ingress::PAYLOAD_SECONDS)
        .await
        .unwrap();
    assert!(
        store
            .list()
            .await
            .unwrap()
            .iter()
            .all(|r| r.payload.is_none())
    );
    worker.abort();
    let _ = worker.await;
    dispatch_test_hooks()
        .lock()
        .unwrap()
        .remove(&(state.home_dir.clone(), event_id.into()));
    worker_test_probes().lock().unwrap().remove(&state.home_dir);
}

#[tokio::test]
async fn aborting_dispatch_worker_drops_heartbeat_and_preserves_uncertain_fence() {
    assert_worker_exit_stops_renewal(false).await;
}

#[tokio::test]
async fn panicking_dispatch_worker_restarts_without_orphaning_heartbeat() {
    assert_worker_exit_stops_renewal(true).await;
}

/// U1: with `line_late_reply = "fail"` an expired reply token is recorded
/// and no request at all reaches the provider (behaviour, not a source scan).
#[tokio::test]
async fn expired_reply_token_with_fail_policy_sends_nothing() {
    let provider = crate::test_channel_provider::TestChannelProvider::start().await;
    let (sent, outcome) = RUN
        .scope(
            std::cell::RefCell::new(delivery::RunReceipt::new(
                0,
                crate::channel_ingress::config::LateReply::Fail,
                false,
            )),
            async {
                let sent = send_reply_rich(
                    &reqwest::Client::new(),
                    &provider.token,
                    "expired",
                    vec![serde_json::json!({"type":"text","text":"answer"})],
                )
                .await;
                (sent, RUN.with(|r| r.borrow().outcome))
            },
        )
        .await;
    assert!(!sent);
    assert_eq!(outcome, Some("reply_token_expired"));
    assert!(provider.requests().is_empty(), "no reply and no push");
}
