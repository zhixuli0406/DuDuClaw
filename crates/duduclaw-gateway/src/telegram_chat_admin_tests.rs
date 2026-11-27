//! RM-143 P14 adapter tests. Intentionally unrun until gateway review.
use super::*;
use crate::approval::{ApprovalStatus, decision_service::tests::Fixture};
use crate::approval::chat_admin::press::PressOutcome;
use crate::decision_action::{DecisionAct, DecisionSource, encode};
use crate::test_channel_provider::TestChannelProvider;

fn callback(id: &str) -> TgCallbackQuery {
    serde_json::from_value(json!({
        "id":"rm143-click", "from":{"id":123456789,"is_bot":false},
        "message":{"message_id":42,"chat":{"id":123456789,"type":"private"}},
        "data":encode(DecisionSource::Approval, DecisionAct::Approve, id)
    })).unwrap()
}

async fn assert_pending(f: &Fixture, id: &crate::approval::ApprovalId) {
    assert_eq!(f.status(id).await, ApprovalStatus::Pending);
    let count: i64 = rusqlite::Connection::open(f.env.home.join("approvals.db")).unwrap().query_row(
        "SELECT count(*) FROM chat_admin_decisions WHERE approval_id=?1",
        [id.as_str()], |row| row.get(0),
    ).unwrap();
    assert_eq!(count, 0);
}

#[test]
fn origin_uses_only_the_pressed_message_and_receiving_account() {
    let mut cb = callback("12345678-1234-4234-8234-123456789abc");
    let origin = callback_press_origin(&cb, Some("support"));
    assert_eq!(origin.message_id.as_deref(), Some("42"));
    assert!(origin.private);
    assert_eq!(origin.account, crate::chat_link_intercept::account_label("telegram", Some("support")));
    assert_eq!(origin.dm, "123456789");
    cb.message.as_mut().unwrap().chat.id = 999;
    assert!(!callback_press_origin(&cb, None).private);
    cb.message = None;
    let origin = callback_press_origin(&cb, None);
    assert_eq!(origin.message_id, None);
    assert!(!origin.private);
    assert!(origin.dm.is_empty());
}

#[tokio::test]
async fn group_press_refused() {
    let f = Fixture::new();
    let id = f.simple(crate::expert_skills::SKILLS_ACTION_KIND).await;
    f.proof(&id).await;
    let ctx = Arc::new(crate::channel_reply::access_gate_tests::reply_ctx(&f.env.home));
    let mut cb = callback(id.as_str());
    // Even a matching chat/principal ID cannot turn a group into a DM.
    cb.message.as_mut().unwrap().chat.chat_type = Some("group".into());
    let origin = callback_press_origin(&cb, None);
    let outcome = crate::decision_notify::route_verified_bound_press(
        &ctx, &crate::approval::DecisionContext {
            channel:"telegram".into(), account_id:"telegram|7".into(),
            conversation_id:origin.dm.clone(), principal_id:"123456789".into(),
        }, cb.data.as_deref().unwrap(), crate::decision_notify::DecisionAccessScope {
            channel_id:Some(&origin.dm), guild_id:None, session_id:None,
        }, &origin,
    ).await.unwrap();
    assert_eq!(outcome.refused_reason, Some("not_private"));
    assert!(outcome.card_text.is_none());
    let provider = TestChannelProvider::start().await;
    handle_callback_query(&cb, &reqwest::Client::new(), &telegram_poll_api_base(&provider.token),
        &ctx, "telegram|7", None).await;
    let calls = provider.requests();
    assert_eq!(calls.len(), 1);
    assert!(calls[0].path.ends_with("/answerCallbackQuery"));
    assert_eq!(calls[0].body["text"], outcome.reply);
    assert_pending(&f, &id).await;
}

#[tokio::test]
async fn missing_message_is_refused_without_edit_or_decision() {
    let f = Fixture::new();
    let id = f.simple(crate::expert_skills::SKILLS_ACTION_KIND).await;
    f.proof(&id).await;
    let ctx = Arc::new(crate::channel_reply::access_gate_tests::reply_ctx(&f.env.home));
    let provider = TestChannelProvider::start().await;
    let mut cb = callback(id.as_str());
    cb.message = None;
    assert_eq!(callback_press_origin(&cb, None).message_id, None);
    handle_callback_query(&cb, &reqwest::Client::new(), &telegram_poll_api_base(&provider.token),
        &ctx, "telegram|7", None).await;
    let calls = provider.requests();
    assert_eq!(calls.len(), 1);
    assert!(calls[0].path.ends_with("/answerCallbackQuery"));
    assert_eq!(calls[0].body["text"], crate::channel_decision_route::DECISION_REFUSED);
    assert_pending(&f, &id).await;
}

#[tokio::test]
async fn expired_card_text_edits_pressed_message_with_receiving_bot_and_no_keyboard() {
    let f = Fixture::new();
    let id = f.simple(crate::expert_skills::SKILLS_ACTION_KIND).await;
    let proof = f.proof(&id).await;
    rusqlite::Connection::open(f.env.home.join("approvals.db")).unwrap().execute(
        "UPDATE chat_admin_cards SET message_id='42', valid_until='2000-01-01T00:00:00Z' WHERE card_id=?1",
        [&proof.card_id],
    ).unwrap();
    let ctx = Arc::new(crate::channel_reply::access_gate_tests::reply_ctx(&f.env.home));
    let provider = TestChannelProvider::start().await;
    handle_callback_query(&callback(id.as_str()), &reqwest::Client::new(),
        &telegram_poll_api_base(&provider.token), &ctx, "telegram|7", None).await;
    let calls = provider.requests();
    assert_eq!(calls.len(), 2);
    assert!(calls[0].path.ends_with("/answerCallbackQuery"));
    assert_eq!(calls[0].body["show_alert"], false);
    assert_eq!(calls[1].path, format!("/bot{}/editMessageText", provider.token));
    assert_eq!(calls[1].body["chat_id"], 123456789);
    assert_eq!(calls[1].body["message_id"], 42);
    assert_eq!(calls[1].body["reply_markup"], json!({"inline_keyboard":[]}));
    assert!(calls[1].body["text"].as_str().unwrap().starts_with("已失效"));
    assert!(calls[1].body.get("parse_mode").is_none());
    assert_pending(&f, &id).await;
}

#[tokio::test]
async fn toast_is_character_bounded_and_only_card_text_requests_an_edit() {
    let cb = callback("12345678-1234-4234-8234-123456789abc");
    let provider = TestChannelProvider::start().await;
    let api = telegram_poll_api_base(&provider.token);
    let client = reqwest::Client::new();
    let text = "界🦀".repeat(150);
    let outcome = PressOutcome { reply:text.clone(), card_text:Some(text.clone()), refused_reason:None };
    reply_to_decision_press(&cb, &client, &api, &outcome).await;
    let calls = provider.requests();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].body["text"], "界🦀".repeat(100));
    assert_eq!(calls[0].body["show_alert"], false);
    assert_eq!(calls[1].body["text"], text);
    assert_eq!(calls[1].body["reply_markup"], json!({"inline_keyboard":[]}));

    for outcome in [PressOutcome::refused("refused".into(), "not_private", false),
        PressOutcome::from_result(Ok("legacy success".into())),
        PressOutcome::from_result(Err("legacy refusal".into()))] {
        reply_to_decision_press(&cb, &client, &api, &outcome).await;
    }
    let calls = provider.requests();
    assert_eq!(calls.len(), 5);
    assert_eq!(calls.iter().filter(|call| call.path.ends_with("/editMessageText")).count(), 1);
    assert_eq!(calls[3].body["text"], "legacy success");
    assert_eq!(calls[4].body["text"], "⚠️ legacy refusal");
}
