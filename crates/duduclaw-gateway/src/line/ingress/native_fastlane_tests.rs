use super::*;
use crate::decision_notify::native_loop_fixture::{NativeCuFixture, install_named_job};
use crate::test_channel_provider::TestChannelProvider;
use base64::Engine;
use std::sync::atomic::{AtomicUsize, Ordering};

fn event(id: &str, text: &str) -> serde_json::Value {
    serde_json::json!({
        "destination": "line-bot-A",
        "events": [
                      {
                          "type": "message",
                          "webhookEventId": id,
                          "replyToken": format!("reply-{id}"),
                          "source": {"type":"group","userId":"human-1","groupId":"group-1"},
                          "message": {"type":"text","id":id,"text":text}
                      }
                  ]
    })
}
async fn send(state: &LineState, value: &serde_json::Value) -> StatusCode {
    let bytes = serde_json::to_vec(value).unwrap();
    let mut mac = HmacSha256::new_from_slice(b"isolated-line-secret").unwrap();
    mac.update(&bytes);
    let mut headers = HeaderMap::new();
    headers.insert(
        "x-line-signature",
        base64::engine::general_purpose::STANDARD
            .encode(mac.finalize().into_bytes())
            .parse()
            .unwrap(),
    );
    handle_line_webhook(state.clone(), &headers, Bytes::from(bytes)).await
}
async fn wait_status(state: &LineState, event_id: &str, wanted: &str) {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if state
                .ingress
                .as_ref()
                .unwrap()
                .list()
                .await
                .unwrap()
                .iter()
                .any(|r| r.event_id == event_id && r.status == wanted)
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("LINE actual worker did not reach expected receipt state");
}
async fn line_fixture(provider: &TestChannelProvider) -> (NativeCuFixture, LineState) {
    let fixture = NativeCuFixture::new(&provider.token).await;
    std::fs::write(
        fixture.home.path().join("config.toml"),
        format!(
            "[channels]\nline_channel_token='{}'\nline_channel_secret='isolated-line-secret'\n",
            provider.token
        ),
    )
    .unwrap();
    let agent_path = fixture.home.path().join("agents/alice/agent.toml");
    let existing: toml::Table = std::fs::read_to_string(&agent_path)
        .unwrap()
        .parse()
        .unwrap();
    // The shared CU fixture deliberately has only CU fields. This
    // adapter test also needs a complete production routing config.
    let mut agent = toml::toml! {
        [agent]
        name="alice"
        display_name="Alice"
        role="main"
        status="active"
        trigger="@alice"
        reports_to=""
        icon="fixture"
        [model]
        preferred="fixture"
        fallback="fixture"
        account_pool=[]
        [container]
        timeout_ms=1000
        [heartbeat]
        enabled=false
        interval_seconds=3600
        max_concurrent_runs=1
        cron=""
        [budget]
        monthly_limit_cents=5000
        warn_threshold_percent=80
        hard_stop=true
        [permissions]
        can_create_agents=false
        can_send_cross_agent=false
        can_modify_own_skills=false
        can_modify_own_soul=false
        can_schedule_tasks=false
        allowed_channels=["line"]
        [evolution]
        enabled=false
    };
    agent.insert("capabilities".into(), existing["capabilities"].clone());
    let _: duduclaw_core::types::AgentConfig =
        toml::from_str(&toml::to_string(&agent).unwrap()).expect("complete routing fixture");
    std::fs::write(agent_path, toml::to_string(&agent).unwrap()).unwrap();
    fixture.ctx.registry.write().await.scan().await.unwrap();
    let state = LineState {
        home_dir: fixture.home.path().to_path_buf(),
        ctx: fixture.ctx.clone(),
        http: reqwest::Client::new(),
        channel_status: fixture.ctx.channel_status.clone(),
        event_tx: fixture.ctx.event_tx.clone(),
        ingress: Some(Arc::new(
            crate::channel_ingress::IngressStore::open(fixture.home.path()).unwrap(),
        )),
    };
    (fixture, state)
}
#[tokio::test]
async fn signed_line_same_chat_confirmation_bypasses_waiting_cu_fifo_and_shares_run() {
    for postback in [false, true] {
        let provider = TestChannelProvider::start().await;
        let (fixture, state) = line_fixture(&provider).await;
        let _cu = install_named_job(
            &provider.token,
            "run cu",
            fixture.work(&format!("line-cu-{}", uuid::Uuid::new_v4())),
        );
        let later = Arc::new(AtomicUsize::new(0));
        let later_count = later.clone();
        let _later = install_named_job(&provider.token, "later", async move {
            later_count.fetch_add(1, Ordering::SeqCst);
        });
        assert_eq!(
            send(&state, &event("initial", "run cu")).await,
            StatusCode::OK
        );
        let initial = state
            .ingress
            .as_ref()
            .unwrap()
            .list()
            .await
            .unwrap()
            .into_iter()
            .find(|r| r.event_id == "initial")
            .unwrap();
        assert_eq!(initial.run_id, initial.id);
        let worker = tokio::spawn(drain_line_ingress(state.clone()));
        let id = fixture.pending_id().await;
        assert_eq!(send(&state, &event("later", "later")).await, StatusCode::OK);
        for (event_id, field, replacement) in [
            ("wrong-user", "userId", "human-2"),
            ("wrong-chat", "groupId", "group-2"),
        ] {
            let mut wrong = event(event_id, &format!("確認 {id}"));
            wrong["events"][0]["source"][field] = replacement.into();
            assert_eq!(send(&state, &wrong).await, StatusCode::OK);
            wait_status(&state, event_id, "completed").await;
            assert_eq!(fixture.executed.load(Ordering::SeqCst), 0);
        }
        if !postback {
            let broker = crate::approval::ApprovalBroker::open(fixture.home.path()).unwrap();
            let original = broker
                .get(&crate::approval::ApprovalId::from(id.clone()))
                .await
                .unwrap()
                .unwrap();
            let payload = serde_json::json!({"options":["A","B"]});
            let mut binding = original.binding.unwrap();
            binding.payload_hash = crate::approval::payload_hash(&payload);
            binding.resume_handler = "workflow_v1".into();
            let question = broker
                .request_bound(
                    crate::approval::RequestKind::Question,
                    "alice",
                    "Choose",
                    payload,
                    binding,
                )
                .await
                .unwrap();
            assert_eq!(
                send(
                    &state,
                    &event("question-approve", &format!("確認 {question}"))
                )
                .await,
                StatusCode::OK
            );
            wait_status(&state, "question-approve", "completed").await;
            assert_eq!(
                broker.get(&question).await.unwrap().unwrap().status,
                crate::approval::ApprovalStatus::Pending
            );
            assert_eq!(
                send(
                    &state,
                    &event("question-answer", &format!("回答 {question} B"))
                )
                .await,
                StatusCode::OK
            );
            wait_status(&state, "question-answer", "completed").await;
            let row = broker.get(&question).await.unwrap().unwrap();
            assert_eq!(row.status, crate::approval::ApprovalStatus::Answered);
            assert_eq!(row.answer, Some(serde_json::json!("B")));
            assert_eq!(fixture.executed.load(Ordering::SeqCst), 0);
            assert_eq!(
                broker
                    .get(&crate::approval::ApprovalId::from(id.clone()))
                    .await
                    .unwrap()
                    .unwrap()
                    .status,
                crate::approval::ApprovalStatus::Pending
            );
        }
        let mut wrong = event("wrong-account", &format!("確認 {id}"));
        wrong["destination"] = "line-bot-B".into();
        assert_eq!(send(&state, &wrong).await, StatusCode::OK);
        wait_status(&state, "wrong-account", "completed").await;
        assert_eq!(fixture.executed.load(Ordering::SeqCst), 0);
        assert_eq!(later.load(Ordering::SeqCst), 0);
        let mut confirmation = event("confirm", &format!("確認 {id}"));
        if postback {
            let data = crate::decision_action::encode(
                crate::decision_action::DecisionSource::Approval,
                crate::decision_action::DecisionAct::Approve,
                &id,
            );
            confirmation["events"][0]["type"] = "postback".into();
            confirmation["events"][0]
                .as_object_mut()
                .unwrap()
                .remove("message");
            confirmation["events"][0]["postback"] = serde_json::json!({"data":data});
        }
        let decision_committed = Arc::new(tokio::sync::Notify::new());
        let decision_release = Arc::new(tokio::sync::Notify::new());
        if postback {
            dispatch_test_hooks().lock().unwrap().insert(
                (state.home_dir.clone(), "decision-after-confirm".into()),
                (decision_committed.clone(), decision_release),
            );
        }
        assert_eq!(send(&state, &confirmation).await, StatusCode::OK);
        assert_eq!(send(&state, &confirmation).await, StatusCode::OK);
        if postback {
            tokio::time::timeout(
                std::time::Duration::from_secs(5),
                decision_committed.notified(),
            )
            .await
            .unwrap();
        }
        fixture.wait_executed().await;
        wait_status(&state, "initial", "completed").await;
        if !postback {
            wait_status(&state, "confirm", "completed").await;
        }
        wait_status(&state, "later", "completed").await;
        assert_eq!(fixture.executed.load(Ordering::SeqCst), 1);
        assert_eq!(later.load(Ordering::SeqCst), 1);
        let broker = crate::approval::ApprovalBroker::open(fixture.home.path()).unwrap();
        let operations = broker.list_operations().await.unwrap();
        assert_eq!(operations.len(), 1);
        assert_eq!(operations[0].run_id, initial.run_id);
        assert_eq!(operations[0].binding.run_origin_kind, "ingress");
        let rows = state.ingress.as_ref().unwrap().list().await.unwrap();
        assert_eq!(rows.iter().filter(|r| r.event_id == "confirm").count(), 1);
        assert!(
            rows.iter()
                .find(|r| r.event_id == "confirm")
                .unwrap()
                .decision_fastlane
        );
        assert!(
            !rows
                .iter()
                .find(|r| r.event_id == "later")
                .unwrap()
                .decision_fastlane
        );
        let requests = provider.requests();
        assert!(requests.iter().any(|r| {
            r.path.ends_with("/push")
                && r.body["to"] == "group-1"
                && r.body["messages"][0]["text"]
                    .as_str()
                    .is_some_and(|s| s.contains(&id))
        }));
        assert_eq!(
            requests
                .iter()
                .any(|r| r.path.ends_with("/reply") && r.body["replyToken"] == "reply-confirm"),
            !postback
        );
        assert!(requests.iter().all(|r| r.authorization.as_deref()
            == Some(format!("Bearer {}", provider.token).as_str())));
        worker.abort();
        assert!(worker.await.unwrap_err().is_cancelled());
        if postback {
            let store = state.ingress.as_ref().unwrap();
            store
                .recover_and_purge(
                    chrono::Utc::now().timestamp()
                        + crate::channel_ingress::PAYLOAD_SECONDS
                        + 1,
                )
                .await
                .unwrap();
            let row = store
                .list()
                .await
                .unwrap()
                .into_iter()
                .find(|r| r.event_id == "confirm")
                .unwrap();
            assert_eq!(row.status, "uncertain");
            assert!(row.payload.is_none());
            let handler =
                crate::handlers::MethodHandler::new(fixture.home.path().to_path_buf()).await;
            let frame = handler
                .handle(
                    "channel_ingress.inspect",
                    serde_json::json!({"ingress_id":row.id}),
                    &duduclaw_auth::UserContext::admin_fallback(),
                )
                .await;
            let crate::protocol::WsFrame::Response {
                ok: true,
                payload: Some(readback),
                ..
            } = frame
            else {
                panic!("fixed decision readback failed: {frame:?}");
            };
            assert_eq!(readback["decision_receipt"]["request_id"], id);
            assert_eq!(readback["decision_receipt"]["status"], "approved");
            assert_eq!(readback["event"]["status"], "uncertain");
            assert_eq!(
                readback["decision_receipt"]["transport_receipt_independent"],
                true
            );
            assert_eq!(fixture.executed.load(Ordering::SeqCst), 1);
            dispatch_test_hooks()
                .lock()
                .unwrap()
                .remove(&(state.home_dir.clone(), "decision-after-confirm".into()));
        }
    }
}
fn postback_event(event_id: &str, request_id: &str, approve: bool) -> serde_json::Value {
    let mut payload = event(event_id, "");
    payload["events"][0]["type"] = "postback".into();
    payload["events"][0]
        .as_object_mut()
        .unwrap()
        .remove("message");
    payload["events"][0]["postback"] = serde_json::json!({
        "data": crate::decision_action::encode(
            crate::decision_action::DecisionSource::Approval,
            if approve {
                crate::decision_action::DecisionAct::Approve
            } else {
                crate::decision_action::DecisionAct::Deny
            },
            request_id
        )
    });
    payload
}
#[tokio::test]
async fn signed_line_legacy_postback_keeps_original_domain_auth_and_corrupt_binding_never_downgrades()
 {
    let provider = TestChannelProvider::start().await;
    let (fixture, state) = line_fixture(&provider).await;
    let broker = Arc::new(crate::approval::ApprovalBroker::open(fixture.home.path()).unwrap());
    let worker = tokio::spawn(drain_line_ingress(state.clone()));
    for approve in [false, true] {
        let id = broker
            .request(
                "alice",
                "mcp_tool",
                "Legacy domain",
                serde_json::json!({"tool":"fixture"}),
                300,
            )
            .await
            .unwrap();
        broker
            .set_notify_target_for_test(&id, "line", "human-1")
            .await
            .unwrap();
        let waiter = {
            let broker = broker.clone();
            let id = id.clone();
            tokio::spawn(async move {
                broker
                    .await_decision(&id, std::time::Duration::from_millis(10))
                    .await
                    .unwrap()
            })
        };
        let event_id = format!("legacy-{approve}");
        assert_eq!(
            send(&state, &postback_event(&event_id, id.as_str(), approve)).await,
            StatusCode::OK
        );
        wait_status(&state, &event_id, "completed").await;
        let wanted = if approve {
            crate::approval::ApprovalStatus::Approved
        } else {
            crate::approval::ApprovalStatus::Denied
        };
        assert_eq!(
            tokio::time::timeout(std::time::Duration::from_secs(2), waiter)
                .await
                .unwrap()
                .unwrap(),
            wanted
        );
        let row = broker.get(&id).await.unwrap().unwrap();
        assert_eq!(row.status, wanted);
        assert!(row.binding.is_none());
    }
    let id = broker
        .request(
            "alice",
            "mcp_tool",
            "Corrupt must refuse",
            serde_json::json!({}),
            300,
        )
        .await
        .unwrap();
    broker
        .set_notify_target_for_test(&id, "line", "human-1")
        .await
        .unwrap();
    let conn = rusqlite::Connection::open(fixture.home.path().join("approvals.db")).unwrap();
    conn.execute(
        "UPDATE approvals SET binding_json='{corrupt' WHERE id=?1",
        rusqlite::params![id.as_str()],
    )
    .unwrap();
    assert_eq!(
        send(&state, &postback_event("corrupt", id.as_str(), true)).await,
        StatusCode::OK
    );
    wait_status(&state, "corrupt", "completed").await;
    let status: String = conn
        .query_row(
            "SELECT status FROM approvals WHERE id=?1",
            rusqlite::params![id.as_str()],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(status, "pending");
    assert!(broker.get(&id).await.is_err());
    assert!(
        broker.list_operations().await.unwrap().is_empty(),
        "legacy replies must not gain bound execution authority"
    );
    assert_eq!(fixture.executed.load(Ordering::SeqCst), 0);
    worker.abort();
    let _ = worker.await;
}
#[tokio::test]
async fn signed_line_native_and_text_decisions_refuse_removed_group_or_blocked_user() {
    for policy in ["channel", "user"] {
        for native in [false, true] {
            let provider = TestChannelProvider::start().await;
            let (fixture, state) = line_fixture(&provider).await;
            let _job = install_named_job(
                &provider.token,
                "run cu",
                fixture.work(&format!("revoke-{}", uuid::Uuid::new_v4())),
            );
            assert_eq!(
                send(&state, &event("initial", "run cu")).await,
                StatusCode::OK
            );
            let worker = tokio::spawn(drain_line_ingress(state.clone()));
            let id = fixture.pending_id().await;
            let (key, value) = if policy == "channel" {
                (keys::ALLOWED_CHANNELS, r#"["other-group"]"#)
            } else {
                (keys::BLOCKED_USERS, r#"["human-1"]"#)
            };
            // Prime the real ReplyContext cache, then revoke through a
            // separate SQL connection to the same production sessions DB.
            fixture
                .ctx
                .channel_settings
                .set("line", "global", key, "[]")
                .await
                .unwrap();
            assert_eq!(
                fixture
                    .ctx
                    .channel_settings
                    .get("line", "global", key)
                    .await
                    .as_deref(),
                Some("[]")
            );
            let settings = crate::channel_settings::ChannelSettingsManager::new(
                &fixture.home.path().join("sessions.db"),
            )
            .unwrap();
            settings.set("line", "global", key, value).await.unwrap();
            let db =
                rusqlite::Connection::open(fixture.home.path().join("sessions.db")).unwrap();
            let stored: String = db
                .query_row(
                    "SELECT value FROM channel_settings WHERE channel_type='line' AND scope_id='global' AND key=?1",
                    rusqlite::params![key],
                    |row| row.get(0)
                )
                .unwrap();
            assert_eq!(
                stored, value,
                "revocation must update the real authoritative row"
            );
            assert_eq!(
                fixture
                    .ctx
                    .channel_settings
                    .get("line", "global", key)
                    .await
                    .as_deref(),
                Some("[]"),
                "the previous ReplyContext cache remains stale before the authenticated handler refresh"
            );
            let decision = if native {
                postback_event("rejected", &id, true)
            } else {
                event("rejected", &format!("確認 {id}"))
            };
            assert_eq!(send(&state, &decision).await, StatusCode::OK);
            wait_status(&state, "rejected", "completed").await;
            let broker = crate::approval::ApprovalBroker::open(fixture.home.path()).unwrap();
            assert_eq!(
                broker
                    .get(&crate::approval::ApprovalId::from(id))
                    .await
                    .unwrap()
                    .unwrap()
                    .status,
                crate::approval::ApprovalStatus::Pending
            );
            assert_eq!(fixture.executed.load(Ordering::SeqCst), 0);
            assert!(
                broker
                    .list_operations()
                    .await
                    .unwrap()
                    .iter()
                    .all(|o| o.state == crate::approval::OperationState::Prepared)
            );
            worker.abort();
            let _ = worker.await;
        }
    }
}
