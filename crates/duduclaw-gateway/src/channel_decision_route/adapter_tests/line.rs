//! P0-B F4 entry tests for LINE (child module of `line`, so the adapter's
//! private functions are reachable). The durable webhook routes a message
//! into the decision lane only when `line_decision_request_id` finds a full
//! request id; these tests drive that classification and the decision lane
//! (`process_line_decision`) with the trusted context the durable worker
//! scopes around it. The general lane's verb-only case is covered in
//! `channel_reply::inner` (`f4_verb_only_tests`).

use super::*;
use crate::channel_decision_route::test_support::{provider_sent_text, seed_decision_users};
use crate::decision_notify::native_loop_fixture::{
    NativeCuFixture, assert_native_approved, assert_native_pending, pending_native_request,
};
use crate::test_channel_provider::TestChannelProvider;

fn text_event(user: &str, text: &str) -> LineEvent {
    serde_json::from_value(serde_json::json!({
        "type": "message",
        "webhookEventId": format!("e-{}", uuid::Uuid::new_v4()),
        "replyToken": format!("r-{}", uuid::Uuid::new_v4()),
        "source": {"type": "group", "userId": user, "groupId": "group-1"},
        "message": {"type": "text", "id": "m1", "text": text}
    }))
    .unwrap()
}

fn group_context(user: &str) -> crate::approval::DecisionContext {
    crate::approval::DecisionContext {
        channel: "line".into(),
        account_id: "line-bot-A".into(),
        conversation_id: "group-1".into(),
        principal_id: user.into(),
    }
}

fn state(fixture: &NativeCuFixture) -> LineState {
    LineState {
        home_dir: fixture.home.path().to_path_buf(),
        ctx: fixture.ctx.clone(),
        http: reqwest::Client::new(),
        channel_status: fixture.ctx.channel_status.clone(),
        event_tx: fixture.ctx.event_tx.clone(),
        ingress: None,
    }
}

/// What the durable worker does for a decision-lane event: scope the
/// verified context, then run the decision lane.
async fn decide(fixture: &NativeCuFixture, provider: &TestChannelProvider, user: &str, text: &str) {
    let event = text_event(user, text);
    let state = state(fixture);
    crate::approval::CURRENT_DECISION_CONTEXT
        .scope(
            Some(group_context(user)),
            process_line_decision(event, &state, &provider.token),
        )
        .await;
}

/// Review M1: only verb + full request id is routed to the decision lane;
/// 「確認」 alone and verb-first sentences stay in the general lane (and so
/// reach the model, see `f4_verb_only_tests`).
#[test]
fn only_full_request_ids_enter_the_decision_lane() {
    let id = uuid::Uuid::new_v4().to_string();
    for text in [
        "確認",
        "取消",
        "approve the Q3 budget",
        "deny it",
        "確認 123",
    ] {
        assert_eq!(
            line_decision_request_id(&text_event("U1", text)),
            None,
            "{text}"
        );
    }
    assert_eq!(
        line_decision_request_id(&text_event("U1", &format!("確認 {id}"))),
        Some(id.clone())
    );
    assert_eq!(
        line_decision_request_id(&text_event("U1", &format!("回答 {id} B"))),
        Some(id)
    );
}

/// Review test gap: with dashboard users configured, only a verified Manager
/// who is also the bound person approves from LINE.
#[tokio::test]
async fn users_db_only_a_verified_manager_approves() {
    let provider = TestChannelProvider::start().await;
    let fixture = NativeCuFixture::new(&provider.token).await;
    seed_decision_users(fixture.home.path(), "line", "U1", "U2");
    let employee = pending_native_request(&fixture, group_context("U1")).await;
    decide(
        &fixture,
        &provider,
        "U1",
        &format!("確認 {}", employee.as_str()),
    )
    .await;
    assert_native_pending(&fixture, &employee).await;
    assert!(provider_sent_text(&provider, "您沒有核准的權限"));

    let provider = TestChannelProvider::start().await;
    let fixture = NativeCuFixture::new(&provider.token).await;
    seed_decision_users(fixture.home.path(), "line", "U1", "U2");
    let manager = pending_native_request(&fixture, group_context("U2")).await;
    decide(
        &fixture,
        &provider,
        "U2",
        &format!("確認 {}", manager.as_str()),
    )
    .await;
    assert_native_approved(&fixture, &manager).await;
}

/// Review L2 on LINE: another member's confirmation and an unknown id read
/// the same, and nothing is decided.
#[tokio::test]
async fn foreign_and_unknown_ids_get_the_same_refusal() {
    let provider = TestChannelProvider::start().await;
    let fixture = NativeCuFixture::new(&provider.token).await;
    let id = pending_native_request(&fixture, group_context("U1")).await;
    decide(&fixture, &provider, "U9", &format!("確認 {}", id.as_str())).await;
    decide(
        &fixture,
        &provider,
        "U1",
        &format!("確認 {}", uuid::Uuid::new_v4()),
    )
    .await;
    let refusals = provider
        .requests()
        .iter()
        .filter(|r| {
            r.body["messages"][0]["text"]
                .as_str()
                .is_some_and(|t| t.contains(crate::channel_decision_route::DECISION_REFUSED))
        })
        .count();
    assert_eq!(refusals, 2);
    assert_native_pending(&fixture, &id).await;
}
