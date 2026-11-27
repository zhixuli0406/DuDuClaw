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
    let outcome = PressOutcome { reply:text.clone(), card_text:Some(text.clone()), card_link:None, refused_reason:None };
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

#[tokio::test]
async fn step_up_replaces_buttons_with_url_without_ticket_in_toast() {
    let provider = TestChannelProvider::start().await;
    let cb = callback("12345678-1234-4234-8234-123456789abc");
    let url = format!("https://dashboard.example.test/approvals/confirm#ticket={}", "a".repeat(64));
    let outcome = PressOutcome { reply:"需要再確認一次".into(), card_text:Some("需要再確認一次".into()),
        card_link:Some(crate::approval::chat_admin::press::CardLink { label:"前往確認".into(), url:url.clone() }),
        refused_reason:Some("step_up_required") };
    reply_to_decision_press(&cb, &reqwest::Client::new(), &telegram_poll_api_base(&provider.token), &outcome).await;
    let calls = provider.requests();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].body["text"], "需要再確認一次");
    assert_eq!(calls[1].body["reply_markup"]["inline_keyboard"], json!([[{"text":"前往確認","url":url}]]));
    assert_eq!(calls[1].body["link_preview_options"]["is_disabled"], true);
}

#[test]
fn totp_event_requires_private_human_unforwarded_sender_and_exact_prompt_reply() {
    use crate::approval::chat_admin::step_up::dm_totp;
    let raw = json!({"message_id":44,"text":"123456","chat":{"id":123456789,"type":"private"},
        "from":{"id":123456789,"is_bot":false},"reply_to_message":{"message_id":43,
            "chat":{"id":123456789,"type":"private"},"text":dm_totp::prompt_text(crate::trigger_route::texts::Lang::ZhTw)}});
    let msg: TgMessage = serde_json::from_value(raw.clone()).unwrap();
    let event = telegram_totp_event(&msg, Some("support"));
    assert!(event.private);
    assert_eq!(event.account, "telegram:support");
    assert_eq!(event.reply_to.as_deref(), Some("43"));
    assert_eq!(event.principal, "123456789");
    assert!(event.prompt_hint);
    for case in ["group", "different_chat", "bot", "unknown_bot", "forward", "legacy_forward", "forward_chat", "forward_name", "forward_date", "sender_chat"] {
        let mut raw = raw.clone();
        match case {
            "group" => raw["chat"]["type"] = json!("group"),
            "different_chat" => raw["chat"]["id"] = json!(999),
            "bot" => raw["from"]["is_bot"] = json!(true),
            "unknown_bot" => raw["from"].as_object_mut().unwrap().remove("is_bot").map(|_| ()).unwrap(),
            "forward" => raw["forward_origin"] = json!({"type":"hidden_user","sender_user_name":"X"}),
            "legacy_forward" => raw["forward_from"] = json!({"id":123}),
            "forward_chat" => raw["forward_from_chat"] = json!({"id":123}),
            "forward_name" => raw["forward_sender_name"] = json!("X"),
            "forward_date" => raw["forward_date"] = json!(1),
            _ => raw["sender_chat"] = json!({"id":123456789,"type":"private"}),
        }
        let msg: TgMessage = serde_json::from_value(raw).unwrap();
        assert!(!telegram_totp_event(&msg, None).private, "{case}");
    }
}

#[tokio::test]
async fn totp_sink_deletes_exact_code_message_on_receiving_bot_and_ignores_failure() {
    use crate::approval::chat_admin::step_up::dm_totp::ReplySink;
    let home = tempfile::tempdir().unwrap();
    let provider = TestChannelProvider::start().await;
    let api = telegram_poll_api_base(&provider.token);
    let client = reqwest::Client::new();
    let msg: TgMessage = serde_json::from_value(json!({"message_id":44,"text":"123456",
        "chat":{"id":123456789,"type":"private"},"from":{"id":123456789,"is_bot":false}})).unwrap();
    let sink = TelegramTotpSink { msg:&msg, client:&client, api:&api, home:home.path() };
    sink.delete_code().await;
    provider.refuse();
    sink.delete_code().await;
    let calls = provider.requests();
    assert_eq!(calls.len(), 2);
    for call in calls {
        assert_eq!(call.path, format!("/bot{}/deleteMessage", provider.token));
        assert_eq!(call.body, json!({"chat_id":123456789,"message_id":44}));
        assert!(!String::from_utf8_lossy(&call.raw_body).contains("123456\""));
    }
}

#[test]
fn quoting_an_undeleted_code_never_reintroduces_it_to_model_context() {
    let msg: TgMessage = serde_json::from_value(json!({"message_id":45,"text":"hello",
        "chat":{"id":123456789,"type":"private"}, "from":{"id":123456789,"is_bot":false},
        "reply_to_message":{"message_id":44,"text":"123456","chat":{"id":123456789,"type":"private"}}})).unwrap();
    let context = build_quoted_context(&msg, "fixture_bot").unwrap();
    assert!(!context.contains("123456"));
    assert!(context.contains("驗證碼已隱藏"));
}

#[tokio::test]
async fn totp_unbound_codes_pass_through_without_provider_requests() {
    let f = Fixture::new();
    let ctx = crate::channel_reply::access_gate_tests::reply_ctx(&f.env.home);
    let provider = TestChannelProvider::start().await;
    let client = reqwest::Client::new();
    let api = telegram_poll_api_base(&provider.token);
    // This principal has no Admin identity and no step-up ticket.
    for chat_type in ["group", "private"] {
        for text in ["123456", "0123456789abcdef0123456789abcdef"] {
            let msg: TgMessage = serde_json::from_value(json!({"message_id":44,"text":text,
                "chat":{"id":777,"type":chat_type},"from":{"id":777,"is_bot":false}})).unwrap();
            assert!(!telegram_totp_intercept(&msg, &client, &api, &ctx, None, "telegram|7").await);
            assert!(provider.requests().is_empty(), "{chat_type}: no delete or reply");
        }
    }
    let msg: TgMessage = serde_json::from_value(json!({"message_id":45,"text":"123456",
        "chat":{"id":-777,"type":"group"},"from":{"id":777,"is_bot":false},
        "reply_to_message":{"message_id":43,"chat":{"id":-777,"type":"group"},
            "text":crate::approval::chat_admin::step_up::dm_totp::prompt_text(crate::trigger_route::texts::Lang::ZhTw)}})).unwrap();
    assert!(!telegram_totp_intercept(&msg, &client, &api, &ctx, None, "telegram|7").await);
    assert!(provider.requests().is_empty());
}
