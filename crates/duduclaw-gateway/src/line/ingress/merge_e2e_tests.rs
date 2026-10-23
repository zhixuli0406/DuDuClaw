//! B10 end to end: signed webhooks → durable inbox → real workers → fake
//! LINE API. Messages sent while the first turn runs are answered in one
//! turn, with the newest message's reply token, and none needs a Push.

use super::*;
use crate::decision_notify::native_loop_fixture::{NativeCuFixture, install_named_job};
use crate::test_channel_provider::TestChannelProvider;
use base64::Engine;

fn one_to_one(id: &str, text: &str) -> serde_json::Value {
    serde_json::json!({
        "destination": "line-bot-A",
        "events": [{
            "type": "message",
            "webhookEventId": id,
            "replyToken": format!("reply-{id}"),
            "deliveryContext": {"isRedelivery": false},
            "source": {"type": "user", "userId": "human-1"},
            "message": {"type": "text", "id": id, "text": text}
        }]
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

async fn wait_until<F: Fn(&[crate::channel_ingress::IngressRow]) -> bool>(state: &LineState, done: F) {
    tokio::time::timeout(TEST_WAIT, async {
        loop {
            let rows = state.ingress.as_ref().unwrap().list().await.unwrap();
            if done(&rows) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("LINE worker did not reach the expected state");
}

fn status<'a>(rows: &'a [crate::channel_ingress::IngressRow], id: &str) -> &'a str {
    rows.iter()
        .find(|r| r.event_id == id)
        .map(|r| r.status.as_str())
        .unwrap_or("")
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
    let existing: toml::Table = std::fs::read_to_string(&agent_path).unwrap().parse().unwrap();
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
async fn messages_sent_during_a_turn_are_answered_together_with_the_newest_token() {
    let provider = TestChannelProvider::start().await;
    let (_fixture, state) = line_fixture(&provider).await;
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let (e, r) = (entered.clone(), release.clone());
    let _first = install_named_job(&provider.token, "第一則", async move {
        e.notify_one();
        r.notified().await;
    });
    let combined = super::merge::combined_text(
        crate::trigger_route::texts::Lang::ZhTw,
        &["第二則".into(), "第三則".into(), "第四則".into()],
    );
    let merged_turns = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let count = merged_turns.clone();
    let _merged = install_named_job(&provider.token, &combined, async move {
        count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    });

    assert_eq!(send(&state, &one_to_one("m1", "第一則")).await, StatusCode::OK);
    let worker = tokio::spawn(drain_line_ingress(state.clone()));
    tokio::time::timeout(TEST_WAIT, entered.notified()).await.unwrap();
    for (id, text) in [("m2", "第二則"), ("m3", "第三則"), ("m4", "第四則")] {
        assert_eq!(send(&state, &one_to_one(id, text)).await, StatusCode::OK);
    }
    // Every waiting message has its snapshot before the first turn ends.
    wait_until(&state, |rows| rows.iter().filter(|r| r.revision != "pending").count() == 4).await;
    release.notify_one();
    wait_until(&state, |rows| status(rows, "m2") == "completed").await;
    worker.abort();

    let rows = state.ingress.as_ref().unwrap().list().await.unwrap();
    assert_eq!(status(&rows, "m1"), "completed");
    assert_eq!(status(&rows, "m3"), "merged");
    assert_eq!(status(&rows, "m4"), "merged");
    assert_eq!(merged_turns.load(std::sync::atomic::Ordering::SeqCst), 1, "one turn for three messages");
    let requests = provider.requests();
    let replies: Vec<_> = requests
        .iter()
        .filter(|r| r.path.ends_with("/message/reply"))
        .map(|r| r.body["replyToken"].as_str().unwrap_or("").to_string())
        .collect();
    assert_eq!(replies, vec!["reply-m1", "reply-m4"], "the newest token answers the folded turn");
    assert!(
        !requests.iter().any(|r| r.path.ends_with("/message/push")),
        "nothing needed the Push quota"
    );
}

#[tokio::test]
async fn a_group_burst_is_still_answered_one_by_one() {
    let provider = TestChannelProvider::start().await;
    let (_fixture, state) = line_fixture(&provider).await;
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let (e, r) = (entered.clone(), release.clone());
    let _first = install_named_job(&provider.token, "群組一", async move {
        e.notify_one();
        r.notified().await;
    });
    let _second = install_named_job(&provider.token, "群組二", async {});
    let _third = install_named_job(&provider.token, "群組三", async {});
    let group = |id: &str, text: &str| {
        let mut v = one_to_one(id, text);
        v["events"][0]["source"] = serde_json::json!({"type": "group", "groupId": "G1", "userId": "human-1"});
        v
    };
    assert_eq!(send(&state, &group("g1", "群組一")).await, StatusCode::OK);
    let worker = tokio::spawn(drain_line_ingress(state.clone()));
    tokio::time::timeout(TEST_WAIT, entered.notified()).await.unwrap();
    assert_eq!(send(&state, &group("g2", "群組二")).await, StatusCode::OK);
    assert_eq!(send(&state, &group("g3", "群組三")).await, StatusCode::OK);
    wait_until(&state, |rows| rows.iter().filter(|r| r.revision != "pending").count() == 3).await;
    release.notify_one();
    wait_until(&state, |rows| ["g1", "g2", "g3"].iter().all(|id| status(rows, id) == "completed")).await;
    worker.abort();
}
