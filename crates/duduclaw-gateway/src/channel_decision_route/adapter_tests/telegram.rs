//! P0-B F4 entry tests for Telegram (child module of `telegram`, so the
//! adapter's private functions are reachable).

use super::*;
use crate::channel_decision_route::DECISION_REFUSED;
use crate::channel_decision_route::test_support::{
    legacy_card, provider_sent_text, seed_decision_users, status_of,
};
use crate::decision_notify::native_loop_fixture::{
    NativeCuFixture, assert_native_approved, assert_native_pending, install_named_job,
    pending_native_request,
};
use crate::test_channel_provider::TestChannelProvider;
use std::sync::atomic::{AtomicUsize, Ordering};

const ACCOUNT: &str = "telegram|7";

fn private_message(id: i64, user: i64, text: &str) -> TgMessage {
    serde_json::from_value(json!({
        "message_id": id,
        "chat": {"id": user, "type": "private"},
        "from": {"id": user, "first_name": "Human"},
        "text": text
    }))
    .unwrap()
}

fn card_reply(id: i64, user: i64, card_message: i64, text: &str) -> TgMessage {
    serde_json::from_value(json!({
        "message_id": id,
        "chat": {"id": user, "type": "private"},
        "from": {"id": user, "first_name": "Human"},
        "text": text,
        "reply_to_message": {
            "message_id": card_message,
            "chat": {"id": user, "type": "private"},
            "from": {"id": 7, "username": "fixturebot", "first_name": "Fixture", "is_bot": true}
        }
    }))
    .unwrap()
}

fn private_context(user: &str) -> crate::approval::DecisionContext {
    crate::approval::DecisionContext {
        channel: "telegram".into(),
        account_id: ACCOUNT.into(),
        conversation_id: user.into(),
        principal_id: user.into(),
    }
}

async fn fastlane(msg: &TgMessage, provider: &TestChannelProvider, f: &NativeCuFixture) -> bool {
    telegram_decision_fastlane(
        msg,
        &reqwest::Client::new(),
        &telegram_poll_api_base(&provider.token),
        &f.ctx,
        ACCOUNT,
        "fixturebot",
    )
    .await
}

async fn process(msg: TgMessage, provider: &TestChannelProvider, f: &NativeCuFixture) {
    process_telegram_message(
        msg,
        reqwest::Client::new(),
        telegram_poll_api_base(&provider.token),
        provider.token.clone(),
        f.ctx.clone(),
        "telegram".into(),
        None,
        ACCOUNT.into(),
        "fixturebot".into(),
    )
    .await;
}

/// Review M1 at the real poll loop: the employee asked "shall I send it?";
/// 「確認」 and "approve the Q3 budget" reach the model, nothing is decided and
/// the bot sends no "request id" error.
#[tokio::test]
async fn poll_loop_passes_verb_only_replies_to_the_model() {
    let provider = TestChannelProvider::start().await;
    for _ in 0..64 {
        provider.enqueue_response(
            "getMe",
            json!({"ok":true,"result":{"id":7,"username":"fixturebot","first_name":"Fixture"}}),
        );
    }
    let fixture = NativeCuFixture::new(&provider.token).await;
    let pending = pending_native_request(&fixture, private_context("11")).await;
    let reached = Arc::new(AtomicUsize::new(0));
    let mut jobs = Vec::new();
    for text in ["確認", "approve the Q3 budget"] {
        let count = reached.clone();
        jobs.push(install_named_job(&provider.token, text, async move {
            count.fetch_add(1, Ordering::SeqCst);
        }));
    }
    let updates: Vec<serde_json::Value> = [(1, "確認"), (2, "approve the Q3 budget")]
        .into_iter()
        .map(|(id, text)| {
            json!({
                "update_id": id,
                "message": {
                    "message_id": id,
                    "chat": {"id": 11, "type": "private"},
                    "from": {"id": 11, "first_name": "Human"},
                    "text": text
                }
            })
        })
        .collect();
    provider.enqueue_response("getUpdates", json!({"ok": true, "result": updates}));
    let poll = tokio::spawn(poll_loop(
        reqwest::Client::new(),
        provider.token.clone(),
        fixture.ctx.clone(),
        "telegram".into(),
        None,
    ));
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while reached.load(Ordering::SeqCst) != 2 {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("verb-only replies never reached the model");
    poll.abort();
    let _ = poll.await;
    assert!(
        !provider
            .requests()
            .iter()
            .any(|r| r.path.ends_with("sendMessage")),
        "the bot must not answer a verb-only reply itself"
    );
    assert!(!provider_sent_text(&provider, DECISION_REFUSED));
    assert_native_pending(&fixture, &pending).await;
    drop(jobs);
}

/// Review M2: replying to an old (WP1.6) approval card with `deny` still
/// decides it, and 「取消」 still gets that card's own answer — the new
/// receiver lane hands both back instead of swallowing them.
#[tokio::test]
async fn old_card_reply_verbs_still_reach_the_wp16_handler() {
    let provider = TestChannelProvider::start().await;
    let fixture = NativeCuFixture::new(&provider.token).await;
    let home = fixture.home.path();
    // No channel token in config: the legacy decision then edits no card on a
    // real platform (collapse needs a token) — only the local provider is hit.
    std::fs::write(home.join("config.toml"), "[channels]\n").unwrap();
    seed_decision_users(home, "telegram", "12", "11");
    let denied = legacy_card(home, "telegram", "11", "9001").await;
    let reply = card_reply(5, 11, 9001, "deny");
    assert!(!fastlane(&reply, &provider, &fixture).await);
    process(reply, &provider, &fixture).await;
    assert_eq!(
        status_of(home, &denied).await,
        crate::approval::ApprovalStatus::Denied
    );

    let kept = legacy_card(home, "telegram", "11", "9002").await;
    let reply = card_reply(6, 11, 9002, "取消");
    assert!(!fastlane(&reply, &provider, &fixture).await);
    process(reply, &provider, &fixture).await;
    assert!(provider_sent_text(&provider, "這張卡不支援「取消」"));
    assert_eq!(
        status_of(home, &kept).await,
        crate::approval::ApprovalStatus::Pending
    );
    assert!(!provider_sent_text(&provider, DECISION_REFUSED));
}

/// Review test gap: with dashboard users configured, the bound person's
/// reply approves only when that person is a verified Manager.
#[tokio::test]
async fn users_db_only_a_verified_manager_approves() {
    let provider = TestChannelProvider::start().await;
    let fixture = NativeCuFixture::new(&provider.token).await;
    seed_decision_users(fixture.home.path(), "telegram", "11", "12");
    let employee_request = pending_native_request(&fixture, private_context("11")).await;
    assert!(
        fastlane(
            &private_message(1, 11, &format!("確認 {}", employee_request.as_str())),
            &provider,
            &fixture
        )
        .await
    );
    assert_native_pending(&fixture, &employee_request).await;
    assert!(provider_sent_text(&provider, "您沒有核准的權限"));

    let provider = TestChannelProvider::start().await;
    let fixture = NativeCuFixture::new(&provider.token).await;
    seed_decision_users(fixture.home.path(), "telegram", "11", "12");
    let manager_request = pending_native_request(&fixture, private_context("12")).await;
    assert!(
        fastlane(
            &private_message(2, 12, &format!("確認 {}", manager_request.as_str())),
            &provider,
            &fixture
        )
        .await
    );
    assert_native_approved(&fixture, &manager_request).await;
}

/// Review L1: anonymous admins, "send as channel" and bots share placeholder
/// ids; even a row naming that id cannot be decided by such a message.
#[tokio::test]
async fn shared_telegram_senders_never_decide() {
    for (from, sender_chat) in [
        (
            json!({"id": 1087968824, "first_name": "Group", "is_bot": true}),
            true,
        ),
        (
            json!({"id": 136817688, "first_name": "Channel", "is_bot": true}),
            true,
        ),
        (json!({"id": 1087968824, "first_name": "Group"}), false),
        (
            json!({"id": 11, "first_name": "Bot", "is_bot": true}),
            false,
        ),
    ] {
        let provider = TestChannelProvider::start().await;
        let fixture = NativeCuFixture::new(&provider.token).await;
        let principal = from["id"].as_i64().unwrap().to_string();
        let id = pending_native_request(
            &fixture,
            crate::approval::DecisionContext {
                channel: "telegram".into(),
                account_id: ACCOUNT.into(),
                conversation_id: "-42".into(),
                principal_id: principal,
            },
        )
        .await;
        let mut message = json!({
            "message_id": 3,
            "chat": {"id": -42, "type": "supergroup"},
            "from": from,
            "text": format!("確認 {}", id.as_str())
        });
        if sender_chat {
            message["sender_chat"] = json!({"id": -42, "type": "supergroup"});
        }
        let message: TgMessage = serde_json::from_value(message).unwrap();
        assert_eq!(telegram_message_principal(&message), "");
        assert!(fastlane(&message, &provider, &fixture).await);
        assert_native_pending(&fixture, &id).await;
        assert!(provider_sent_text(&provider, DECISION_REFUSED));
    }
}

/// Review L3: `@bot 確認 <id>` is recognised by the receiver lane itself, so
/// it never waits on the conversation FIFO behind the turn it releases.
#[tokio::test]
async fn mention_prefixed_confirmation_is_handled_on_the_receiver() {
    let provider = TestChannelProvider::start().await;
    let fixture = NativeCuFixture::new(&provider.token).await;
    let id = pending_native_request(&fixture, private_context("11")).await;
    let msg = private_message(4, 11, &format!("@fixturebot 確認 {}", id.as_str()));
    assert!(fastlane(&msg, &provider, &fixture).await);
    assert_native_approved(&fixture, &id).await;
}

/// F5-C (review F4-L8): forwarding a message that reads "確認 <id>" is not
/// the forwarder deciding; it is refused and nothing is decided.
#[tokio::test]
async fn forwarded_confirmation_never_decides() {
    let provider = TestChannelProvider::start().await;
    let fixture = NativeCuFixture::new(&provider.token).await;
    let id = pending_native_request(&fixture, private_context("11")).await;
    let msg: TgMessage = serde_json::from_value(json!({
        "message_id": 7,
        "chat": {"id": 11, "type": "private"},
        "from": {"id": 11, "first_name": "Human"},
        "forward_origin": {"type": "user", "sender_user": {"id": 22, "first_name": "Other"}},
        "text": format!("確認 {}", id.as_str())
    }))
    .unwrap();
    assert_eq!(telegram_message_principal(&msg), "");
    assert!(fastlane(&msg, &provider, &fixture).await);
    assert_native_pending(&fixture, &id).await;
    assert!(provider_sent_text(&provider, DECISION_REFUSED));
}

/// F5-C (review F4-L4): the leading mention is matched case-insensitively;
/// a longer name that merely starts with this bot's name is not this bot.
#[tokio::test]
async fn leading_mention_is_case_insensitive_and_anchored() {
    let provider = TestChannelProvider::start().await;
    let fixture = NativeCuFixture::new(&provider.token).await;
    let id = pending_native_request(&fixture, private_context("11")).await;
    let other = private_message(8, 11, &format!("@fixturebotx 確認 {}", id.as_str()));
    assert!(!fastlane(&other, &provider, &fixture).await);
    assert_native_pending(&fixture, &id).await;
    let msg = private_message(9, 11, &format!("@FixtureBot 確認 {}", id.as_str()));
    assert!(fastlane(&msg, &provider, &fixture).await);
    assert_native_approved(&fixture, &id).await;
}

/// F5-C (review F4-L5): a confirmation with extra spaces and a closing 。 is
/// still handled on the receiver, not sent to the model.
#[tokio::test]
async fn near_miss_confirmation_is_still_a_decision() {
    let provider = TestChannelProvider::start().await;
    let fixture = NativeCuFixture::new(&provider.token).await;
    let id = pending_native_request(&fixture, private_context("11")).await;
    let msg = private_message(10, 11, &format!("確認\u{3000} {}。", id.as_str()));
    assert!(fastlane(&msg, &provider, &fixture).await);
    assert_native_approved(&fixture, &id).await;
}
