//! P0-B F4 entry tests for Slack (child module of `slack`, so the adapter's
//! private functions are reachable). Each test drives the same two calls the
//! Socket Mode loop makes for an `events_api` envelope: the receiver lane
//! (`slack_decision_fastlane`) and, when it declines, `handle_event`.

use super::*;
use crate::channel_decision_route::test_support::{
    legacy_card, provider_sent_text, seed_decision_users, status_of,
};
use crate::channel_decision_route::{DECISION_REFUSED, SLACK_IDENTITY_UNAVAILABLE};
use crate::decision_notify::native_loop_fixture::{
    NativeCuFixture, assert_native_approved, assert_native_pending, install_named_job,
    pending_native_request,
};
use crate::test_channel_provider::TestChannelProvider;

fn account() -> SlackDecisionAccount {
    SlackDecisionAccount {
        account_id: "slack|T1|UBOT|A1".into(),
        team_id: "T1".into(),
        app_id: "A1".into(),
    }
}

fn payload(
    channel: &str,
    channel_type: &str,
    user: &str,
    thread: Option<&str>,
    text: &str,
) -> serde_json::Value {
    let mut event = json!({
        "type": "message",
        "channel": channel,
        "channel_type": channel_type,
        "user": user,
        "ts": format!("1700.{}", uuid::Uuid::new_v4().simple()),
        "text": text
    });
    if let Some(thread) = thread {
        event["thread_ts"] = json!(thread);
    }
    json!({"team_id": "T1", "api_app_id": "A1", "event": event})
}

fn dm_context(user: &str) -> crate::approval::DecisionContext {
    crate::approval::DecisionContext {
        channel: "slack".into(),
        account_id: account().account_id,
        conversation_id: "D1".into(),
        principal_id: user.into(),
    }
}

/// Mirrors the Socket Mode loop: the receiver lane first, `handle_event` when
/// it declines. Returns whether the lane consumed the message.
async fn route(
    payload: &serde_json::Value,
    provider: &TestChannelProvider,
    fixture: &NativeCuFixture,
    account: Option<&SlackDecisionAccount>,
) -> bool {
    let http = reqwest::Client::new();
    if slack_decision_fastlane(payload, &provider.token, &fixture.ctx, &http, account).await {
        return true;
    }
    handle_event(
        payload,
        &provider.token,
        "UBOT",
        &fixture.ctx,
        &http,
        None,
        account,
    )
    .await;
    false
}

/// Review M1: 「確認」 and "approve the Q3 budget" reach the model; nothing is
/// decided and no decision error is posted.
#[tokio::test]
async fn verb_only_messages_reach_the_model() {
    let provider = TestChannelProvider::start().await;
    let fixture = NativeCuFixture::new(&provider.token).await;
    let pending = pending_native_request(&fixture, dm_context("H1")).await;
    let acct = account();
    for text in ["確認", "approve the Q3 budget"] {
        let reached = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = reached.clone();
        let _job = install_named_job(&provider.token, text, async move {
            flag.store(true, std::sync::atomic::Ordering::SeqCst);
        });
        let consumed = route(
            &payload("D1", "im", "H1", None, text),
            &provider,
            &fixture,
            Some(&acct),
        )
        .await;
        assert!(!consumed, "{text}");
        assert!(reached.load(std::sync::atomic::Ordering::SeqCst), "{text}");
    }
    assert!(!provider_sent_text(&provider, DECISION_REFUSED));
    assert!(!provider_sent_text(&provider, "請求編號"));
    assert_native_pending(&fixture, &pending).await;
}

/// Review M2: a thread reply `deny` under an old approval card decides it,
/// and 「取消」 gets the card's own WP1.6 answer.
#[tokio::test]
async fn old_card_thread_replies_still_reach_the_wp16_handler() {
    let provider = TestChannelProvider::start().await;
    let fixture = NativeCuFixture::new(&provider.token).await;
    let home = fixture.home.path();
    std::fs::write(home.join("config.toml"), "[channels]\n").unwrap();
    seed_decision_users(home, "slack", "H2", "H1");
    let acct = account();
    let denied = legacy_card(home, "slack", "C1", "1700.0001").await;
    assert!(
        !route(
            &payload("C1", "channel", "H1", Some("1700.0001"), "deny"),
            &provider,
            &fixture,
            Some(&acct)
        )
        .await
    );
    assert_eq!(
        status_of(home, &denied).await,
        crate::approval::ApprovalStatus::Denied
    );
    let kept = legacy_card(home, "slack", "C1", "1700.0002").await;
    assert!(
        !route(
            &payload("C1", "channel", "H1", Some("1700.0002"), "取消"),
            &provider,
            &fixture,
            Some(&acct)
        )
        .await
    );
    assert!(provider_sent_text(&provider, "這張卡不支援「取消」"));
    assert_eq!(
        status_of(home, &kept).await,
        crate::approval::ApprovalStatus::Pending
    );
}

/// Review test gap: with dashboard users configured, only a verified Manager
/// who is also the bound person approves from Slack.
#[tokio::test]
async fn users_db_only_a_verified_manager_approves() {
    let acct = account();
    let provider = TestChannelProvider::start().await;
    let fixture = NativeCuFixture::new(&provider.token).await;
    seed_decision_users(fixture.home.path(), "slack", "H1", "H2");
    let employee = pending_native_request(&fixture, dm_context("H1")).await;
    let text = format!("確認 {}", employee.as_str());
    assert!(
        route(
            &payload("D1", "im", "H1", None, &text),
            &provider,
            &fixture,
            Some(&acct)
        )
        .await
    );
    assert_native_pending(&fixture, &employee).await;
    assert!(provider_sent_text(&provider, "您沒有核准的權限"));

    let provider = TestChannelProvider::start().await;
    let fixture = NativeCuFixture::new(&provider.token).await;
    seed_decision_users(fixture.home.path(), "slack", "H1", "H2");
    let manager = pending_native_request(&fixture, dm_context("H2")).await;
    let text = format!("確認 {}", manager.as_str());
    assert!(
        route(
            &payload("D1", "im", "H2", None, &text),
            &provider,
            &fixture,
            Some(&acct)
        )
        .await
    );
    assert_native_approved(&fixture, &manager).await;
}

/// Review L7: when `bots.info` is refused for a missing scope the failure
/// names the scope, and a decision sent to that bot gets a plain Chinese
/// explanation instead of an identity error; nothing is decided.
#[tokio::test]
async fn missing_users_read_scope_is_named_and_explained() {
    let provider = TestChannelProvider::start().await;
    provider.enqueue_response(
        "auth.test",
        json!({"ok": true, "team_id": "T1", "user_id": "UBOT", "bot_id": "B1"}),
    );
    provider.enqueue_response(
        "bots.info",
        json!({"ok": false, "error": "missing_scope", "needed": "users:read"}),
    );
    let failure =
        verified_slack_decision_account(&reqwest::Client::new(), &provider.token, "slack")
            .await
            .err()
            .expect("missing scope must not verify");
    assert_eq!(failure.step, "bots.info");
    assert_eq!(failure.error, "missing_scope");
    assert_eq!(failure.needed.as_deref(), Some("users:read"));

    let fixture = NativeCuFixture::new(&provider.token).await;
    let pending = pending_native_request(&fixture, dm_context("H1")).await;
    let text = format!("確認 {}", pending.as_str());
    assert!(
        route(
            &payload("D1", "im", "H1", None, &text),
            &provider,
            &fixture,
            None
        )
        .await
    );
    assert!(provider_sent_text(&provider, SLACK_IDENTITY_UNAVAILABLE));
    assert_native_pending(&fixture, &pending).await;
}

/// F5-C (review F4-L3): the missing-scope explanation is only for senders the
/// channel settings admit; a blocked sender gets no answer at all.
#[tokio::test]
async fn missing_scope_explanation_waits_for_the_access_check() {
    let provider = TestChannelProvider::start().await;
    let fixture = NativeCuFixture::new(&provider.token).await;
    fixture
        .ctx
        .channel_settings
        .set("slack", "global", "blocked_users", r#"["H1"]"#)
        .await
        .unwrap();
    let text = format!("確認 {}", uuid::Uuid::new_v4());
    assert!(
        route(
            &payload("D1", "im", "H1", None, &text),
            &provider,
            &fixture,
            None
        )
        .await
    );
    assert!(!provider_sent_text(&provider, SLACK_IDENTITY_UNAVAILABLE));
    assert!(!provider_sent_text(&provider, DECISION_REFUSED));
    assert!(
        route(
            &payload("D1", "im", "H2", None, &text),
            &provider,
            &fixture,
            None
        )
        .await
    );
    assert!(provider_sent_text(&provider, SLACK_IDENTITY_UNAVAILABLE));
}
