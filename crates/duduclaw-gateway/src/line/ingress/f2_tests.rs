//! F2 regression tests: late replies, progress, document notices, narrowed
//! snapshots, database faults, concurrency and a real process kill through
//! the webhook handler and the worker.
use super::*;
use crate::protocol::WsFrame;
use crate::test_channel_provider::TestChannelProvider;
use base64::Engine;

const SECRET: &str = "test-secret";

/// A LINE state over an existing home (registry scanned from disk).
pub(super) async fn state_for(home: &Path) -> LineState {
    let mut registry = duduclaw_agent::AgentRegistry::new(home.join("agents"));
    registry.scan().await.unwrap();
    let sessions =
        Arc::new(crate::session::SessionManager::new(&home.join("sessions.db")).unwrap());
    let (event_tx, _) = tokio::sync::broadcast::channel(16);
    let channel_status = Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new()));
    let ctx = Arc::new(ReplyContext::new(
        Arc::new(tokio::sync::RwLock::new(registry)),
        home.to_path_buf(),
        sessions,
        channel_status.clone(),
        event_tx.clone(),
    ));
    LineState {
        home_dir: home.to_path_buf(),
        ctx,
        http: reqwest::Client::new(),
        channel_status,
        event_tx,
        ingress: Some(Arc::new(
            crate::channel_ingress::IngressStore::open(home).unwrap(),
        )),
    }
}

pub(super) async fn create_agent(home: &Path, name: &str) {
    let handler = crate::handlers::MethodHandler::new(home.to_path_buf()).await;
    let frame = handler
        .handle(
            "agents.create",
            serde_json::json!({"name": name, "display_name": name}),
            &duduclaw_auth::UserContext::admin_fallback(),
        )
        .await;
    assert!(
        matches!(frame, WsFrame::Response { ok: true, .. }),
        "{frame:?}"
    );
}

pub(super) fn edit_agent(home: &Path, name: &str, edit: impl FnOnce(&mut toml::Table)) {
    let path = home.join("agents").join(name).join("agent.toml");
    let mut cfg: toml::Table = std::fs::read_to_string(&path).unwrap().parse().unwrap();
    edit(&mut cfg);
    std::fs::write(path, toml::to_string(&cfg).unwrap()).unwrap();
}

pub(super) fn write_config(home: &Path, token: &str, extra: &str) {
    std::fs::write(
        home.join("config.toml"),
        format!(
            "[channels]\nline_channel_token='{token}'\nline_channel_secret='{SECRET}'\n{extra}"
        ),
    )
    .unwrap();
}

/// One employee routed for LINE, credentials `token`.
pub(super) async fn fixture_in(home: &Path, token: &str, extra: &str) -> LineState {
    create_agent(home, "line-agent").await;
    write_config(home, token, extra);
    edit_agent(home, "line-agent", |cfg| {
        cfg["permissions"].as_table_mut().unwrap().insert(
            "allowed_channels".into(),
            toml::Value::Array(vec![toml::Value::String("line".into())]),
        );
    });
    state_for(home).await
}

pub(super) fn envelope(id: &str, kind: &str) -> serde_json::Value {
    let mut event = serde_json::json!({
        "type": kind,
        "webhookEventId": id,
        "replyToken": format!("reply-{id}"),
        "source": {"type": "user", "userId": "Utest"}
    });
    if kind == "message" {
        event["message"] = serde_json::json!({"type": "text", "id": id, "text": "/status"});
    }
    serde_json::json!({"destination": "bot-account", "events": [event]})
}

pub(super) fn signed(value: &serde_json::Value) -> (HeaderMap, Bytes) {
    let body = serde_json::to_vec(value).unwrap();
    let mut mac = HmacSha256::new_from_slice(SECRET.as_bytes()).unwrap();
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

pub(super) async fn post(state: &LineState, value: &serde_json::Value) -> StatusCode {
    let (h, b) = signed(value);
    handle_line_webhook(state.clone(), &h, b).await
}

pub(super) async fn wait_for(
    state: &LineState,
    event_id: &str,
    wanted: &[&str],
) -> crate::channel_ingress::IngressRow {
    let store = state.ingress.as_ref().unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            if let Some(row) = store
                .list()
                .await
                .unwrap()
                .into_iter()
                .find(|r| r.event_id == event_id && wanted.contains(&r.status.as_str()))
            {
                return row;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("event did not reach the expected status")
}

pub(super) async fn age_events(state: &LineState, seconds: i64) {
    let store = state.ingress.as_ref().unwrap();
    store
        .connection()
        .lock()
        .await
        .execute("UPDATE ingress SET received_at=received_at-?1", [seconds])
        .unwrap();
}

#[tokio::test]
async fn twenty_concurrent_deliveries_of_one_event_run_once() {
    let provider = TestChannelProvider::start().await;
    let dir = tempfile::tempdir().unwrap();
    let state = fixture_in(dir.path(), &provider.token, "").await;
    let envelope = envelope("burst", "message");
    let mut set = tokio::task::JoinSet::new();
    for _ in 0..20 {
        let (state, envelope) = (state.clone(), envelope.clone());
        set.spawn(async move { post(&state, &envelope).await });
    }
    while let Some(status) = set.join_next().await {
        assert_eq!(status.unwrap(), StatusCode::OK);
    }
    assert_eq!(
        state.ingress.as_ref().unwrap().list().await.unwrap().len(),
        1
    );
    let worker = tokio::spawn(drain_line_ingress(state.clone()));
    let row = wait_for(&state, "burst", &["completed", "undelivered", "uncertain"]).await;
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    worker.abort();
    assert_eq!(row.attempt, 1);
    let replies = provider
        .requests()
        .iter()
        .filter(|r| r.path.ends_with("/message/reply") || r.path.ends_with("/message/push"))
        .count();
    assert_eq!(replies, 1, "one accepted event, one answer");
}

#[tokio::test]
async fn read_only_and_busy_database_never_acknowledge() {
    let dir = tempfile::tempdir().unwrap();
    let state = fixture_in(dir.path(), "p0-test-unused", "").await;
    let store = state.ingress.as_ref().unwrap();
    store.simulate_read_only().await;
    assert_eq!(
        post(&state, &envelope("ro", "follow")).await,
        StatusCode::SERVICE_UNAVAILABLE
    );

    let busy = state_for(dir.path()).await;
    busy.ingress
        .as_ref()
        .unwrap()
        .simulate_busy_timeout(50)
        .await;
    let holder = rusqlite::Connection::open(dir.path().join("channel_ingress.db")).unwrap();
    holder.execute_batch("BEGIN IMMEDIATE;").unwrap();
    assert_eq!(
        post(&busy, &envelope("busy", "follow")).await,
        StatusCode::SERVICE_UNAVAILABLE
    );
    holder.execute_batch("COMMIT;").unwrap();
    assert_eq!(
        post(&busy, &envelope("busy", "follow")).await,
        StatusCode::OK
    );
    assert_eq!(
        busy.ingress.as_ref().unwrap().list().await.unwrap().len(),
        1
    );
}

#[tokio::test]
async fn queue_wait_past_the_reply_window_pushes_to_the_same_conversation() {
    let provider = TestChannelProvider::start().await;
    let dir = tempfile::tempdir().unwrap();
    let state = fixture_in(dir.path(), &provider.token, "").await;
    assert_eq!(
        post(&state, &envelope("late", "message")).await,
        StatusCode::OK
    );
    age_events(&state, 120).await;
    let worker = tokio::spawn(drain_line_ingress(state.clone()));
    let row = wait_for(&state, "late", &["completed", "undelivered", "uncertain"]).await;
    worker.abort();
    assert_eq!(row.status, "completed", "{:?}", row.reason);
    let requests = provider.requests();
    assert!(!requests.iter().any(|r| r.path.ends_with("/message/reply")));
    let pushes: Vec<_> = requests
        .iter()
        .filter(|r| r.path.ends_with("/message/push"))
        .collect();
    assert_eq!(pushes.len(), 1);
    assert_eq!(pushes[0].body["to"], "Utest");
    let inspected = state
        .ingress
        .as_ref()
        .unwrap()
        .inspect(&row.id, None, None)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(inspected["attempts"][0]["delivered_via"], "push");
}

#[tokio::test]
async fn queue_wait_with_fail_policy_runs_nothing_and_sends_nothing() {
    let provider = TestChannelProvider::start().await;
    let dir = tempfile::tempdir().unwrap();
    let state = fixture_in(
        dir.path(),
        &provider.token,
        "[channel_ingress]\nline_late_reply='fail'\n",
    )
    .await;
    assert_eq!(
        post(&state, &envelope("late", "message")).await,
        StatusCode::OK
    );
    age_events(&state, 120).await;
    let worker = tokio::spawn(drain_line_ingress(state.clone()));
    let row = wait_for(&state, "late", &["failed_before_dispatch", "completed"]).await;
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    worker.abort();
    assert_eq!(row.status, "failed_before_dispatch");
    assert_eq!(row.reason.as_deref(), Some("late_reply_expired"));
    assert!(provider.requests().is_empty(), "no reply, no push");
    let inspected = state
        .ingress
        .as_ref()
        .unwrap()
        .inspect(&row.id, None, None)
        .await
        .unwrap()
        .unwrap();
    assert!(
        inspected["attempts"].as_array().unwrap().is_empty(),
        "never crossed the dispatch boundary"
    );
}

/// A row admitted and in `dispatching`, with the binding a run would hold.
pub(super) async fn dispatching_binding(state: &LineState, id: &str) -> (Binding, LineEvent) {
    let raw = envelope(id, "follow");
    assert_eq!(post(state, &raw).await, StatusCode::OK);
    let store = state.ingress.as_ref().unwrap();
    let event: LineEvent = serde_json::from_value(raw["events"][0].clone()).unwrap();
    let rev = line_revision(state, &event, None).await.unwrap();
    let pending = store.list().await.unwrap().remove(0);
    store
        .store_snapshot(
            &pending.id,
            &pending.authorization_revision,
            &rev.route,
            &rev.authority,
        )
        .await
        .unwrap();
    let row = store
        .claim(chrono::Utc::now().timestamp())
        .await
        .unwrap()
        .unwrap();
    assert!(
        store
            .transition(&row, "claimed", "dispatching", None)
            .await
            .unwrap()
    );
    let binding = Binding {
        state: state.clone(),
        revision: rev.route,
        authorization: rev.authority,
        payload: serde_json::json!({"destination": "bot-account", "event": raw["events"][0]}),
        row,
    };
    (binding, event)
}

#[tokio::test]
async fn failed_progress_push_never_changes_the_event_status() {
    let provider = TestChannelProvider::start().await;
    let dir = tempfile::tempdir().unwrap();
    let state = fixture_in(dir.path(), &provider.token, "").await;
    let (binding, event) = dispatching_binding(&state, "progress").await;
    let ledger = Arc::new(delivery::ProgressLedger::default());
    provider.refuse();
    let outcome = RUN
        .scope(
            std::cell::RefCell::new(delivery::RunReceipt::new(
                chrono::Utc::now().timestamp() + 60,
                crate::channel_ingress::config::LateReply::Push,
                false,
            )),
            PROGRESS.scope(
                ledger.clone(),
                INGRESS_BINDING.scope(binding, async {
                    let callback = line_progress_callback(&state, &event, &provider.token).unwrap();
                    callback(crate::channel_reply::ProgressEvent::Keepalive);
                    RUN.with(|r| r.borrow().outcome)
                }),
            ),
        )
        .await;
    let note = ledger
        .settle(std::time::Duration::from_secs(5))
        .await
        .unwrap();
    assert!(note.contains("failed=1"), "{note}");
    assert!(
        provider
            .requests()
            .iter()
            .any(|r| r.path.ends_with("/message/push"))
    );
    assert_eq!(outcome, None);
    assert_eq!(delivery::final_status(outcome), "completed");
}

#[tokio::test]
async fn document_notice_joins_the_answer_instead_of_pushing() {
    let dir = tempfile::tempdir().unwrap();
    let state = fixture_in(dir.path(), "p0-test-unused", "").await;
    let agent_dir = dir.path().join("agents/line-agent");
    let file = agent_dir.join("report.csv");
    std::fs::write(&file, "a,b\n1,2\n").unwrap();
    let collector = LineNoticeCollector::default();
    let reply = format!("報告好了\n📎DELIVER: {}", file.display());
    let text =
        crate::office_docs::process_deliverables(&reply, &agent_dir, &state.home_dir, &collector)
            .await;
    let answer = collector.append_to(text);
    assert!(answer.contains("報告好了"));
    assert!(answer.contains("report.csv"), "{answer}");
    assert!(!answer.contains("📎DELIVER:"));
    assert!(collector.take().is_empty(), "notices are consumed once");
}

#[tokio::test]
async fn preset_resolved_configuration_is_part_of_the_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let state = fixture_in(dir.path(), "p0-test-unused", "").await;
    let event: LineEvent =
        serde_json::from_value(envelope("p", "follow")["events"][0].clone()).unwrap();
    let before = line_revision(&state, &event, None).await.unwrap();
    let mut resolved: toml::Table =
        std::fs::read_to_string(dir.path().join("agents/line-agent/agent.toml"))
            .unwrap()
            .parse()
            .unwrap();
    resolved["permissions"]
        .as_table_mut()
        .unwrap()
        .insert("can_schedule_tasks".into(), toml::Value::Boolean(false));
    resolved["budget"]
        .as_table_mut()
        .unwrap()
        .insert("monthly_limit_cents".into(), toml::Value::Integer(3));
    std::fs::create_dir_all(dir.path().join("agent_resolved")).unwrap();
    std::fs::write(
        dir.path().join("agent_resolved/line-agent.toml"),
        toml::to_string(&resolved).unwrap(),
    )
    .unwrap();
    let with_preset = line_revision(&state, &event, None).await.unwrap();
    assert_eq!(before.route, with_preset.route);
    assert_ne!(before.authority, with_preset.authority);
    std::fs::remove_file(dir.path().join("agent_resolved/line-agent.toml")).unwrap();
    assert_eq!(line_revision(&state, &event, None).await.unwrap(), before);
}

#[tokio::test]
async fn unrelated_employees_do_not_move_the_snapshot_but_the_routed_one_does() {
    let dir = tempfile::tempdir().unwrap();
    let state = fixture_in(dir.path(), "p0-test-unused", "").await;
    let event: LineEvent =
        serde_json::from_value(envelope("u", "follow")["events"][0].clone()).unwrap();
    let before = line_revision(&state, &event, None).await.unwrap();
    assert_eq!(before.agent, "line-agent");
    create_agent(dir.path(), "zz-other").await;
    state.ctx.registry.write().await.scan().await.unwrap();
    edit_agent(dir.path(), "zz-other", |cfg| {
        cfg["budget"]
            .as_table_mut()
            .unwrap()
            .insert("monthly_limit_cents".into(), toml::Value::Integer(1));
    });
    assert_eq!(line_revision(&state, &event, None).await.unwrap(), before);
    // A broken unrelated employee neither blocks acknowledgement nor the snapshot.
    std::fs::write(dir.path().join("agents/zz-other/agent.toml"), "not = [toml").unwrap();
    assert_eq!(post(&state, &envelope("u", "follow")).await, StatusCode::OK);
    assert_eq!(line_revision(&state, &event, None).await.unwrap(), before);
    // The routed employee's own authority still moves it.
    edit_agent(dir.path(), "line-agent", |cfg| {
        cfg["budget"]
            .as_table_mut()
            .unwrap()
            .insert("monthly_limit_cents".into(), toml::Value::Integer(7));
    });
    let after = line_revision(&state, &event, None).await.unwrap();
    assert_eq!(after.route, before.route);
    assert_ne!(after.authority, before.authority);
}

#[tokio::test]
async fn unreadable_authority_backs_off_before_quarantine() {
    let dir = tempfile::tempdir().unwrap();
    let state = fixture_in(dir.path(), "p0-test-unused", "").await;
    assert_eq!(post(&state, &envelope("b", "follow")).await, StatusCode::OK);
    // F5 N1: only a snapshotted event is claimable; let the snapshot land.
    let store = state.ingress.as_ref().unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while store.list().await.unwrap()[0].snapshot_pending() {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    // The routed employee's file becomes unreadable (a half-written save).
    std::fs::write(dir.path().join("agents/line-agent/agent.toml"), "half = [").unwrap();
    let worker = tokio::spawn(drain_line_worker(state.clone(), IngressLane::Normal));
    let row = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let row = store.list().await.unwrap().remove(0);
            if row.unavailable_count > 0 {
                return row;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    worker.abort();
    assert_eq!(row.status, "ready", "a read failure is not a change");
    assert!(row.retry_at.is_some());
    // Repeated read failures end in quarantine with a retryable reason.
    let mut row = row;
    for _ in 1..crate::channel_ingress::UNAVAILABLE_LIMIT {
        let now = row.retry_at.unwrap();
        let claimed = store.claim(now).await.unwrap().unwrap();
        store
            .defer_unavailable(&claimed, "agent_config_invalid", now)
            .await
            .unwrap();
        row = store.list().await.unwrap().remove(0);
        if row.status == "quarantined" {
            break;
        }
    }
    assert_eq!(row.status, "quarantined");
    assert_eq!(row.reason.as_deref(), Some("revalidation_unavailable"));
}

/// Re-entered by the parent test and killed with the OS at a real boundary.
#[tokio::test]
async fn crash_child_process() {
    let Ok(root) = std::env::var("DUDU_TEST_LINE_CRASH_ROOT") else {
        return;
    };
    let point = std::env::var("DUDU_TEST_LINE_CRASH_POINT").unwrap();
    let home = std::path::Path::new(&root);
    let state = fixture_in(home, "p0-test-unused", "").await;
    assert_eq!(
        post(&state, &envelope("crash", "follow")).await,
        StatusCode::OK
    );
    if point == "dispatching" {
        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        dispatch_test_hooks().lock().unwrap().insert(
            (state.home_dir.clone(), "crash".into()),
            (entered.clone(), release),
        );
        let _worker = tokio::spawn(drain_line_ingress(state.clone()));
        entered.notified().await;
    }
    std::fs::write(home.join("kill-ready"), "ready").unwrap();
    tokio::time::sleep(std::time::Duration::from_secs(30)).await;
}

#[tokio::test]
async fn os_kill_through_webhook_and_worker_never_loses_or_replays() {
    for point in ["after_ack", "dispatching"] {
        let dir = tempfile::tempdir().unwrap();
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "line::ingress::f2_tests::crash_child_process",
                "--test-threads=1",
            ])
            .env("DUDU_TEST_LINE_CRASH_ROOT", dir.path())
            .env("DUDU_TEST_LINE_CRASH_POINT", point)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        for _ in 0..1000 {
            if dir.path().join("kill-ready").exists() {
                break;
            }
            if let Some(status) = child.try_wait().unwrap() {
                panic!("crash child exited early at {point}: {status}");
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        let ready = dir.path().join("kill-ready").exists();
        child.kill().unwrap();
        child.wait().unwrap();
        assert!(ready, "crash child did not reach {point}");
        let state = state_for(dir.path()).await;
        let store = state.ingress.as_ref().unwrap().clone();
        assert_eq!(
            store.list().await.unwrap().len(),
            1,
            "acknowledged event survives"
        );
        if point == "after_ack" {
            let worker = tokio::spawn(drain_line_ingress(state.clone()));
            let row = wait_for(&state, "crash", &["completed"]).await;
            worker.abort();
            assert_eq!(row.attempt, 1);
        } else {
            let later = chrono::Utc::now().timestamp() + crate::channel_ingress::LEASE_SECONDS + 5;
            store.recover_and_purge(later).await.unwrap();
            assert_eq!(store.list().await.unwrap()[0].status, "uncertain");
            assert!(
                store.claim(later).await.unwrap().is_none(),
                "never replayed automatically"
            );
        }
    }
}

#[test]
fn reply_route_follows_the_late_reply_setting_and_rerun_start() {
    use crate::channel_ingress::config::LateReply;
    use delivery::{DeliveryRoute, RunReceipt};
    let fresh = RunReceipt::new(100, LateReply::Push, false);
    assert_eq!(fresh.route(100), DeliveryRoute::Reply);
    assert_eq!(fresh.route(101), DeliveryRoute::Push);
    let strict = RunReceipt::new(100, LateReply::Fail, false);
    assert_eq!(strict.route(100), DeliveryRoute::Reply);
    assert_eq!(strict.route(101), DeliveryRoute::Expired);
    // A rerun never reuses the old token when Push is allowed; under "fail"
    // the original window still applies (the token is never renewed).
    assert_eq!(
        RunReceipt::new(500, LateReply::Push, true).route(10),
        DeliveryRoute::Push
    );
    assert_eq!(
        RunReceipt::new(100, LateReply::Fail, true).route(101),
        DeliveryRoute::Expired
    );
}

#[tokio::test]
async fn a_possibly_delivered_outcome_is_never_downgraded() {
    let outcome = RUN
        .scope(
            std::cell::RefCell::new(delivery::RunReceipt::new(
                0,
                crate::channel_ingress::config::LateReply::Push,
                false,
            )),
            async {
                record_reply_failure("reply_delivery_uncertain");
                record_reply_failure("reply_rejected");
                RUN.with(|r| r.borrow().outcome)
            },
        )
        .await;
    assert_eq!(outcome, Some("reply_delivery_uncertain"));
    assert_eq!(delivery::final_status(outcome), "uncertain");
    assert_eq!(
        delivery::final_status(Some("reply_rejected")),
        "undelivered"
    );
}

/// A rerun approved while Push was allowed, then the setting switched to
/// "fail": the run is not executed and nothing is sent.
#[tokio::test]
async fn rerun_after_switching_to_fail_policy_runs_nothing() {
    let provider = TestChannelProvider::start().await;
    let dir = tempfile::tempdir().unwrap();
    let state = fixture_in(dir.path(), &provider.token, "").await;
    assert_eq!(
        post(&state, &envelope("again", "message")).await,
        StatusCode::OK
    );
    age_events(&state, 120).await;
    let store = state.ingress.as_ref().unwrap();
    // Let the post-commit snapshot land first, so the CAS below is stable.
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while store.list().await.unwrap()[0].snapshot_pending() {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let row = store
        .claim(chrono::Utc::now().timestamp())
        .await
        .unwrap()
        .unwrap();
    assert!(
        store
            .transition(&row, "claimed", "dispatching", None)
            .await
            .unwrap()
    );
    assert!(
        store
            .transition(&row, "dispatching", "undelivered", Some("reply_rejected"))
            .await
            .unwrap()
    );
    store
        .resolve_request(&crate::channel_ingress::ResolveRequest {
            id: &row.id,
            expected_revision: &row.revision,
            expected_attempt: row.attempt,
            action: "rerun",
            confirm_duplicate_risk: true,
            actor: "dashboard-admin",
            note: "customer asked again",
            provider_receipt: None,
            now: chrono::Utc::now().timestamp(),
            late_reply: crate::channel_ingress::config::LateReply::Push,
        })
        .await
        .unwrap();
    write_config(
        dir.path(),
        &provider.token,
        "[channel_ingress]\nline_late_reply='fail'\n",
    );
    let worker = tokio::spawn(drain_line_ingress(state.clone()));
    let done = wait_for(
        &state,
        "again",
        &["failed_before_dispatch", "completed", "undelivered"],
    )
    .await;
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    worker.abort();
    assert_eq!(done.status, "failed_before_dispatch");
    assert_eq!(done.reason.as_deref(), Some("late_reply_expired"));
    assert!(provider.requests().is_empty(), "no reply, no push");
}

/// After a device restore, waiting events are held before any worker runs,
/// the operator is told once, and nothing is sent.
#[tokio::test]
async fn restored_backup_events_are_held_before_any_worker_claims() {
    let provider = TestChannelProvider::start().await;
    let dir = tempfile::tempdir().unwrap();
    let state = fixture_in(dir.path(), &provider.token, "").await;
    for id in ["r1", "r2"] {
        assert_eq!(post(&state, &envelope(id, "message")).await, StatusCode::OK);
    }
    drop(state);
    // What `backup_restore::perform_pending_restore_swap` leaves behind.
    std::fs::write(dir.path().join(crate::channel_ingress::RESTORE_MARKER), "t").unwrap();
    let restored = state_for(dir.path()).await;
    let worker = tokio::spawn(drain_line_ingress(restored.clone()));
    tokio::time::sleep(std::time::Duration::from_millis(600)).await;
    worker.abort();
    let rows = restored.ingress.as_ref().unwrap().list().await.unwrap();
    assert_eq!(rows.len(), 2);
    for row in &rows {
        assert_eq!(row.status, "quarantined");
        assert_eq!(row.reason.as_deref(), Some("restored_from_backup"));
        assert_eq!(row.attempt, 0, "no worker claimed it");
    }
    assert!(provider.requests().is_empty(), "nothing sent");
    assert!(
        !dir.path()
            .join(crate::channel_ingress::RESTORE_MARKER)
            .exists()
    );
    let store = crate::task_store::TaskStore::open(dir.path()).unwrap();
    let (activity, _) = store
        .list_activity(None, Some("channel_ingress_restored_held"), 10, 0)
        .await
        .unwrap();
    assert_eq!(activity.len(), 1);
    // Held events cannot take a plain retry; a confirmed rerun is allowed.
    let row = &rows[0];
    let base = crate::channel_ingress::ResolveRequest {
        id: &row.id,
        expected_revision: &row.revision,
        expected_attempt: row.attempt,
        action: "retry",
        confirm_duplicate_risk: false,
        actor: "dashboard-admin",
        note: "checked the old device",
        provider_receipt: None,
        now: chrono::Utc::now().timestamp(),
        late_reply: crate::channel_ingress::config::LateReply::Push,
    };
    let s = restored.ingress.as_ref().unwrap();
    assert!(
        s.resolve_request(&base)
            .await
            .unwrap_err()
            .contains("rerun")
    );
    s.resolve_request(&crate::channel_ingress::ResolveRequest {
        action: "rerun",
        confirm_duplicate_risk: true,
        ..base
    })
    .await
    .unwrap();
}
