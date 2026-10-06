//! P0-B F4 entry tests for Discord (child module of `discord`, so the
//! adapter's private functions are reachable). All messages are direct
//! messages, which need no channel lookup on the platform.

use super::*;
use crate::channel_decision_route::DECISION_REFUSED;
use crate::channel_decision_route::DiscordLane;
use crate::channel_decision_route::test_support::{
    legacy_card, provider_sent_text, seed_decision_users, status_of,
};
use crate::decision_notify::native_loop_fixture::{
    NativeCuFixture, assert_native_approved, assert_native_pending, pending_native_request,
};
use crate::test_channel_provider::TestChannelProvider;

fn dm(author: &str, channel: &str, content: &str) -> Value {
    json!({
        "id": format!("m-{}", uuid::Uuid::new_v4()),
        "channel_id": channel,
        "author": {"id": author, "bot": false},
        "content": content
    })
}

fn dm_context(author: &str, channel: &str) -> crate::approval::DecisionContext {
    crate::approval::DecisionContext {
        channel: "discord".into(),
        account_id: "A1".into(),
        conversation_id: channel.into(),
        principal_id: author.into(),
    }
}

async fn deliver(data: &Value, provider: &TestChannelProvider, fixture: &NativeCuFixture) {
    handle_message_create(
        data,
        "BOT",
        &reqwest::Client::new(),
        &provider.token,
        &fixture.ctx,
        None,
        "A1",
    )
    .await;
}

/// Review M1: 「確認」 and "approve the Q3 budget" reach ordinary dispatch
/// (the model), nothing is decided, and the bot posts no decision error.
#[tokio::test]
async fn verb_only_messages_reach_ordinary_dispatch() {
    for text in ["確認", "approve the Q3 budget", "<@BOT> 確認"] {
        let provider = TestChannelProvider::start().await;
        let fixture = NativeCuFixture::new(&provider.token).await;
        let pending = pending_native_request(&fixture, dm_context("H1", "C1")).await;
        let records = super::ordinary_dispatch_probe::install(&provider.token);
        deliver(&dm("H1", "C1", text), &provider, &fixture).await;
        assert_eq!(
            *records.lock().unwrap(),
            vec!["discord:C1".to_owned()],
            "{text}"
        );
        assert!(!provider_sent_text(&provider, DECISION_REFUSED), "{text}");
        assert!(!provider_sent_text(&provider, "請求編號"), "{text}");
        assert_native_pending(&fixture, &pending).await;
        super::ordinary_dispatch_probe::remove(&provider.token);
    }
}

/// Review M2: replying to an old approval card with `deny` decides it and
/// 「取消」 gets the card's own WP1.6 answer; neither is swallowed.
#[tokio::test]
async fn old_card_reply_verbs_still_reach_the_wp16_handler() {
    let provider = TestChannelProvider::start().await;
    let fixture = NativeCuFixture::new(&provider.token).await;
    let home = fixture.home.path();
    std::fs::write(home.join("config.toml"), "[channels]\n").unwrap();
    seed_decision_users(home, "discord", "H2", "H1");
    // Safety net: anything that is not consumed stops at the probe instead
    // of reaching a real model.
    let records = super::ordinary_dispatch_probe::install(&provider.token);
    let reply = |text: &str, card: &str| {
        let mut data = dm("H1", "C1", text);
        data["referenced_message"] = json!({"id": card, "author": {"id": "BOT"}});
        data
    };
    let denied = legacy_card(home, "discord", "C1", "9001").await;
    deliver(&reply("deny", "9001"), &provider, &fixture).await;
    assert_eq!(
        status_of(home, &denied).await,
        crate::approval::ApprovalStatus::Denied
    );
    let kept = legacy_card(home, "discord", "C1", "9002").await;
    deliver(&reply("取消", "9002"), &provider, &fixture).await;
    assert!(provider_sent_text(&provider, "這張卡不支援「取消」"));
    assert_eq!(
        status_of(home, &kept).await,
        crate::approval::ApprovalStatus::Pending
    );
    assert!(records.lock().unwrap().is_empty());
    super::ordinary_dispatch_probe::remove(&provider.token);
}

/// Review M3: with every ordinary permit held by long replies, the real
/// handler still takes a decision permit and decides the request.
#[tokio::test]
async fn confirmation_decides_while_ordinary_work_is_saturated() {
    let permits = Arc::new(crate::channel_decision_route::DiscordPermits::new(10, 4));
    let mut held = Vec::new();
    for _ in 0..10 {
        held.push(permits.acquire(DiscordLane::General).await.unwrap());
    }
    let provider = TestChannelProvider::start().await;
    let fixture = NativeCuFixture::new(&provider.token).await;
    let id = pending_native_request(&fixture, dm_context("H1", "C1")).await;
    let data = dm("H1", "C1", &format!("<@BOT> 確認 {}", id.as_str()));
    tokio::time::timeout(
        std::time::Duration::from_secs(3),
        crate::channel_decision_route::with_test_discord_permits(
            permits.clone(),
            deliver(&data, &provider, &fixture),
        ),
    )
    .await
    .expect("confirmation queued behind ordinary work");
    assert_native_approved(&fixture, &id).await;
    drop(held);
}

fn with_attachments(mut data: Value) -> Value {
    data["attachments"] = json!([
        {"url": "https://127.0.0.1/attachments/big.bin",
         "filename": "big.bin", "content_type": "application/octet-stream",
         "size": 20 * 1024 * 1024},
        {"url": "https://127.0.0.1/attachments/big2.bin",
         "filename": "big2.bin", "content_type": "application/octet-stream",
         "size": 20 * 1024 * 1024}
    ]);
    data
}

/// Review F4-M1: a decision-shaped message with large attachments never
/// downloads them and holds a decision permit only for its short refusal; a
/// sender the channel refuses never takes a decision permit at all. The real
/// confirmation that follows still decides in time.
#[tokio::test]
async fn fake_decisions_with_attachments_cannot_hold_the_decision_lane() {
    let provider = TestChannelProvider::start().await;
    let fixture = NativeCuFixture::new(&provider.token).await;
    let id = pending_native_request(&fixture, dm_context("H1", "C1")).await;
    // One decision permit, so any message that kept it would block the next.
    let permits = Arc::new(crate::channel_decision_route::DiscordPermits::new(10, 1));
    let run = |data: Value| {
        let permits = permits.clone();
        let provider = &provider;
        let fixture = &fixture;
        async move {
            tokio::time::timeout(
                std::time::Duration::from_secs(3),
                crate::channel_decision_route::with_test_discord_permits(
                    permits,
                    deliver(&data, provider, fixture),
                ),
            )
            .await
            .expect("a decision-shaped message must finish promptly")
        }
    };
    // Admitted member, random id, two 20 MiB attachments.
    for _ in 0..4 {
        let fake = with_attachments(dm("H1", "C1", &format!("確認 {}", uuid::Uuid::new_v4())));
        run(fake).await;
    }
    assert_eq!(
        super::attachment_download_probe::count(&provider.token),
        0,
        "decision messages must not download attachments"
    );
    assert_eq!(permits.available(DiscordLane::Decision), 1);

    // A sender the channel settings refuse: while the only decision permit
    // is held elsewhere, the message still finishes (it never waits for one).
    fixture
        .ctx
        .channel_settings
        .set(
            "discord",
            "global",
            "allowed_users",
            r#"["H1","discord:C1"]"#,
        )
        .await
        .unwrap();
    let held = permits.acquire(DiscordLane::Decision).await.unwrap();
    run(with_attachments(dm(
        "H9",
        "C1",
        &format!("確認 {}", uuid::Uuid::new_v4()),
    )))
    .await;
    drop(held);
    assert_eq!(super::attachment_download_probe::count(&provider.token), 0);
    assert_native_pending(&fixture, &id).await;

    // The real confirmation still goes through.
    run(dm("H1", "C1", &format!("確認 {}", id.as_str()))).await;
    assert_native_approved(&fixture, &id).await;
}

/// Review F4-M1: an ordinary message still downloads its attachments, inside
/// the per-download time limit (the counter proves the download path ran).
#[tokio::test]
async fn ordinary_messages_still_download_attachments() {
    let provider = TestChannelProvider::start().await;
    let fixture = NativeCuFixture::new(&provider.token).await;
    let records = super::ordinary_dispatch_probe::install(&provider.token);
    let mut data = with_attachments(dm("H1", "C1", "請看附件"));
    data["attachments"] = json!([data["attachments"][0].clone()]);
    tokio::time::timeout(
        std::time::Duration::from_secs(40),
        deliver(&data, &provider, &fixture),
    )
    .await
    .expect("attachment download must be time-limited");
    assert_eq!(super::attachment_download_probe::count(&provider.token), 1);
    assert_eq!(*records.lock().unwrap(), vec!["discord:C1".to_owned()]);
    super::ordinary_dispatch_probe::remove(&provider.token);
}

/// Review test gap: with dashboard users configured, only a verified Manager
/// who is also the bound person approves from Discord.
#[tokio::test]
async fn users_db_only_a_verified_manager_approves() {
    let provider = TestChannelProvider::start().await;
    let fixture = NativeCuFixture::new(&provider.token).await;
    seed_decision_users(fixture.home.path(), "discord", "H1", "H2");
    let employee = pending_native_request(&fixture, dm_context("H1", "C1")).await;
    deliver(
        &dm("H1", "C1", &format!("確認 {}", employee.as_str())),
        &provider,
        &fixture,
    )
    .await;
    assert_native_pending(&fixture, &employee).await;
    assert!(provider_sent_text(&provider, "您沒有核准的權限"));

    let provider = TestChannelProvider::start().await;
    let fixture = NativeCuFixture::new(&provider.token).await;
    seed_decision_users(fixture.home.path(), "discord", "H1", "H2");
    let manager = pending_native_request(&fixture, dm_context("H2", "C2")).await;
    deliver(
        &dm("H2", "C2", &format!("確認 {}", manager.as_str())),
        &provider,
        &fixture,
    )
    .await;
    assert_native_approved(&fixture, &manager).await;
}
