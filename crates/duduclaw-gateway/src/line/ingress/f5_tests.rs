//! F5-B regression tests: snapshot before claim (N1), redelivery and a
//! refused reply token (N2), unreadable revalidation (N3).
use super::f2_tests::{
    age_events, dispatching_binding, edit_agent, envelope, fixture_in, post, wait_for,
};
use super::*;
use crate::channel_ingress::config::LateReply;
use crate::test_channel_provider::TestChannelProvider;

fn redelivered(id: &str) -> serde_json::Value {
    let mut env = envelope(id, "message");
    env["events"][0]["deliveryContext"] = serde_json::json!({"isRedelivery": true});
    env
}

fn paths(provider: &TestChannelProvider) -> Vec<String> {
    provider.requests().into_iter().map(|r| r.path).collect()
}

#[test]
fn reply_token_deadline_counts_from_the_event_and_skips_redeliveries() {
    let event = |v: serde_json::Value| serde_json::from_value::<LineEvent>(v).unwrap();
    let base = serde_json::json!({"type": "message", "replyToken": "t"});
    assert_eq!(
        delivery::reply_token_deadline(1_000, &event(base.clone())),
        1_060
    );
    let mut older = base.clone();
    older["timestamp"] = serde_json::json!(990_000);
    assert_eq!(delivery::reply_token_deadline(1_000, &event(older)), 1_050);
    let mut newer = base.clone();
    newer["timestamp"] = serde_json::json!(5_000_000);
    assert_eq!(delivery::reply_token_deadline(1_000, &event(newer)), 1_060);
    let mut again = base;
    again["deliveryContext"] = serde_json::json!({"isRedelivery": true});
    assert_eq!(
        delivery::reply_token_deadline(1_000, &event(again)),
        i64::MIN
    );
}

#[tokio::test]
async fn redelivered_webhook_never_uses_its_reply_token_and_pushes() {
    let provider = TestChannelProvider::start().await;
    let dir = tempfile::tempdir().unwrap();
    let state = fixture_in(dir.path(), &provider.token, "").await;
    assert_eq!(post(&state, &redelivered("again")).await, StatusCode::OK);
    let worker = tokio::spawn(drain_line_ingress(state.clone()));
    let row = wait_for(&state, "again", &["completed", "undelivered", "uncertain"]).await;
    worker.abort();
    assert_eq!(row.status, "completed", "{:?}", row.reason);
    let sent = paths(&provider);
    assert!(
        !sent.iter().any(|p| p.ends_with("/message/reply")),
        "{sent:?}"
    );
    assert_eq!(
        sent.iter().filter(|p| p.ends_with("/message/push")).count(),
        1
    );
}

#[tokio::test]
async fn redelivered_webhook_under_fail_policy_runs_nothing() {
    let provider = TestChannelProvider::start().await;
    let dir = tempfile::tempdir().unwrap();
    let state = fixture_in(
        dir.path(),
        &provider.token,
        "[channel_ingress]\nline_late_reply='fail'\n",
    )
    .await;
    assert_eq!(post(&state, &redelivered("again-f")).await, StatusCode::OK);
    let worker = tokio::spawn(drain_line_ingress(state.clone()));
    let row = wait_for(&state, "again-f", &["failed_before_dispatch", "completed"]).await;
    worker.abort();
    assert_eq!(row.status, "failed_before_dispatch");
    assert_eq!(
        row.reason.as_deref(),
        Some("redelivered_reply_token_not_used")
    );
    assert!(provider.requests().is_empty());
}

fn invalid_token(provider: &TestChannelProvider) {
    provider.enqueue_response(
        "/message/reply",
        serde_json::json!({"__status": 400, "message": "Invalid reply token"}),
    );
}

#[tokio::test]
async fn refused_reply_token_falls_back_to_push_when_allowed() {
    let provider = TestChannelProvider::start().await;
    let dir = tempfile::tempdir().unwrap();
    let state = fixture_in(dir.path(), &provider.token, "").await;
    invalid_token(&provider);
    assert_eq!(
        post(&state, &envelope("tok", "message")).await,
        StatusCode::OK
    );
    let worker = tokio::spawn(drain_line_ingress(state.clone()));
    let row = wait_for(&state, "tok", &["completed", "undelivered", "uncertain"]).await;
    worker.abort();
    assert_eq!(row.status, "completed", "{:?}", row.reason);
    let sent = paths(&provider);
    let reply = sent
        .iter()
        .position(|p| p.ends_with("/message/reply"))
        .unwrap();
    let push = sent
        .iter()
        .position(|p| p.ends_with("/message/push"))
        .unwrap();
    assert!(reply < push);
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
async fn refused_reply_token_under_fail_policy_stays_undelivered() {
    let provider = TestChannelProvider::start().await;
    let dir = tempfile::tempdir().unwrap();
    let state = fixture_in(
        dir.path(),
        &provider.token,
        "[channel_ingress]\nline_late_reply='fail'\n",
    )
    .await;
    invalid_token(&provider);
    assert_eq!(
        post(&state, &envelope("tok-f", "message")).await,
        StatusCode::OK
    );
    let worker = tokio::spawn(drain_line_ingress(state.clone()));
    let row = wait_for(&state, "tok-f", &["completed", "undelivered", "uncertain"]).await;
    worker.abort();
    assert_eq!(row.status, "undelivered");
    assert_eq!(row.reason.as_deref(), Some("reply_token_invalid"));
    assert!(
        !paths(&provider)
            .iter()
            .any(|p| p.ends_with("/message/push"))
    );
}

async fn deliver_once(binding: Binding, token: &str) -> Option<&'static str> {
    RUN.scope(
        std::cell::RefCell::new(delivery::RunReceipt::new(
            chrono::Utc::now().timestamp() + 60,
            LateReply::Push,
            false,
        )),
        INGRESS_BINDING.scope(binding, async {
            let sent = deliver(
                &reqwest::Client::new(),
                token,
                "reply-x",
                vec![serde_json::json!({"type": "text", "text": "hi"})],
            )
            .await;
            assert!(!sent);
            RUN.with(|r| r.borrow().outcome)
        }),
    )
    .await
}

#[tokio::test]
async fn unreadable_revalidation_is_not_reported_as_a_change() {
    let provider = TestChannelProvider::start().await;
    let dir = tempfile::tempdir().unwrap();
    let state = fixture_in(dir.path(), &provider.token, "").await;
    let (binding, _) = dispatching_binding(&state, "reval").await;
    let agent = dir.path().join("agents/line-agent/agent.toml");
    let good = std::fs::read_to_string(&agent).unwrap();
    std::fs::write(&agent, "half = [").unwrap();
    assert_eq!(
        deliver_once(binding.clone(), &provider.token).await,
        Some("revalidation_unavailable")
    );
    std::fs::write(&agent, good).unwrap();
    edit_agent(dir.path(), "line-agent", |cfg| {
        cfg["budget"]
            .as_table_mut()
            .unwrap()
            .insert("monthly_limit_cents".into(), toml::Value::Integer(9));
    });
    assert_eq!(
        deliver_once(binding, &provider.token).await,
        Some("authorization_changed_before_delivery")
    );
    assert!(provider.requests().is_empty(), "nothing sent either way");
}

#[tokio::test]
async fn an_event_is_not_claimable_until_its_snapshot_is_taken() {
    let dir = tempfile::tempdir().unwrap();
    let state = fixture_in(dir.path(), "p0-test-unused", "").await;
    let agent = dir.path().join("agents/line-agent/agent.toml");
    let good = std::fs::read_to_string(&agent).unwrap();
    std::fs::write(&agent, "half = [").unwrap();
    assert_eq!(
        post(&state, &envelope("snap", "follow")).await,
        StatusCode::OK
    );
    let store = state.ingress.as_ref().unwrap();
    upkeep::snapshot_pass(&state, store).await;
    let row = store.list().await.unwrap().remove(0);
    assert!(row.snapshot_pending());
    assert!(row.unavailable_count >= 1, "a read failure backs off");
    assert_eq!(row.status, "ready");
    let far = chrono::Utc::now().timestamp() + 3_600;
    assert!(
        store.claim(far).await.unwrap().is_none(),
        "never claimed while pending"
    );
    std::fs::write(&agent, good).unwrap();
    store
        .connection()
        .lock()
        .await
        .execute("UPDATE ingress SET retry_at=0", [])
        .unwrap();
    upkeep::snapshot_pass(&state, store).await;
    assert!(!store.list().await.unwrap()[0].snapshot_pending());
    assert!(
        store
            .claim(chrono::Utc::now().timestamp())
            .await
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn a_pending_event_from_before_a_long_outage_is_held_not_adopted() {
    let dir = tempfile::tempdir().unwrap();
    let state = fixture_in(dir.path(), "p0-test-unused", "").await;
    // Accepted, then the gateway "stops" before the snapshot.
    let store = state.ingress.as_ref().unwrap();
    store
        .append(
            &[crate::channel_ingress::AcceptedEvent {
                decision_fastlane: false,
                decision_binding: None,
                event_id: "old".into(),
                account: "bot-account".into(),
                revision: crate::channel_ingress::PENDING_REVISION.into(),
                authorization_revision: "pending:whatever".into(),
                conversation: "Utest".into(),
                payload: serde_json::json!({
                    "destination": "bot-account",
                    "event": envelope("old", "follow")["events"][0],
                })
                .to_string(),
            }],
            10,
        )
        .await
        .unwrap();
    age_events(&state, 3_600).await;
    upkeep::snapshot_pass(&state, store).await;
    let row = store.list().await.unwrap().remove(0);
    assert_eq!(row.status, "quarantined");
    assert_eq!(row.reason.as_deref(), Some("snapshot_unavailable"));
}
