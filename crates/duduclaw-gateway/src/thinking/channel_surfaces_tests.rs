//! L5: the presenter against a local fixture standing in for each platform
//! API (no real Telegram / Discord / Slack call).

use super::*;
use crate::test_channel_provider::TestChannelProvider;
use crate::thinking::channel_presenter::ChannelThinking;
use crate::thinking::{ThinkingEvent, ThinkingKind, ThinkingPhase};
use crate::trigger_route::texts::Lang;
use duduclaw_core::show_thinking::ShowThinking;
use std::sync::Arc;

const SECRET_FREE_SUMMARY: &str = "先比對兩份報表〔已遮蔽·EMAIL〕";

fn summary_events() -> Vec<ThinkingEvent> {
    let mut start = ThinkingEvent::new("t0", ThinkingPhase::Start, ThinkingKind::Summary, "claude_cli");
    start.started_at = Some(crate::thinking::now_ms());
    let end = ThinkingEvent {
        text: Some(SECRET_FREE_SUMMARY.into()),
        line: Some(SECRET_FREE_SUMMARY.into()),
        ..ThinkingEvent::new("t0", ThinkingPhase::End, ThinkingKind::Summary, "claude_cli")
    };
    vec![start, end]
}

async fn run(p: &Arc<ChannelThinking>, show: ShowThinking) {
    p.set_show(show);
    for ev in summary_events() {
        p.observe(&ev);
    }
    // Let the status task post its first message.
    for _ in 0..50 {
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        if p.is_status_posted().await {
            break;
        }
    }
    assert!(p.finish().await.is_none());
}

#[tokio::test]
async fn telegram_posts_one_status_and_edits_it_into_an_expandable_summary() {
    let provider = TestChannelProvider::start().await;
    provider.enqueue_response("sendMessage", serde_json::json!({"ok": true, "result": {"message_id": 77}}));
    let api_base = crate::test_channel_provider::url(
        &provider.token,
        &format!("https://api.telegram.org/bot{}", provider.token),
    );
    let surface = TelegramStatus {
        http: reqwest::Client::new(),
        api_base,
        chat_id: 42,
        thread_id: None,
    };
    let p = ChannelThinking::new("telegram", false, Lang::ZhTw, Some(Arc::new(surface)));
    run(&p, ShowThinking::Summary).await;
    let reqs = provider.requests();
    let sends: Vec<_> = reqs.iter().filter(|r| r.path.ends_with("/sendMessage")).collect();
    assert_eq!(sends.len(), 1, "exactly one status message: {reqs:?}");
    let last = reqs.last().unwrap();
    assert!(last.path.ends_with("/editMessageText"), "{reqs:?}");
    assert_eq!(last.body["message_id"], 77);
    assert_eq!(last.body["parse_mode"], "HTML");
    let text = last.body["text"].as_str().unwrap();
    assert!(text.starts_with("💭 思考了"), "{text}");
    assert!(text.contains("<blockquote expandable>先比對兩份報表〔已遮蔽·EMAIL〕</blockquote>"), "{text}");
}

#[tokio::test]
async fn telegram_status_audience_requests_carry_no_thinking_text() {
    let provider = TestChannelProvider::start().await;
    provider.enqueue_response("sendMessage", serde_json::json!({"ok": true, "result": {"message_id": 5}}));
    let api_base = crate::test_channel_provider::url(
        &provider.token,
        &format!("https://api.telegram.org/bot{}", provider.token),
    );
    let surface = TelegramStatus {
        http: reqwest::Client::new(),
        api_base,
        chat_id: -100,
        thread_id: Some(9),
    };
    let p = ChannelThinking::new("telegram", true, Lang::ZhTw, Some(Arc::new(surface)));
    run(&p, ShowThinking::Status).await;
    let reqs = provider.requests();
    assert!(!reqs.is_empty());
    for r in &reqs {
        let body = r.body.to_string();
        assert!(!body.contains("先比對"), "no thinking text for a timer-only audience: {body}");
    }
    assert_eq!(reqs[0].body["message_thread_id"], 9);
}

#[tokio::test]
async fn discord_status_disables_mentions_and_uses_a_spoiler() {
    let provider = TestChannelProvider::start().await;
    provider.enqueue_response("/messages", serde_json::json!({"id": "900"}));
    let surface = DiscordStatus {
        http: reqwest::Client::new(),
        token: provider.token.clone(),
        channel_id: "c1".into(),
    };
    let p = ChannelThinking::new("discord", false, Lang::ZhTw, Some(Arc::new(surface)));
    run(&p, ShowThinking::Summary).await;
    let reqs = provider.requests();
    assert_eq!(reqs.iter().filter(|r| r.path.ends_with("/channels/c1/messages")).count(), 1, "{reqs:?}");
    let last = reqs.last().unwrap();
    assert!(last.path.ends_with("/channels/c1/messages/900"), "{reqs:?}");
    assert_eq!(last.body["allowed_mentions"]["parse"], serde_json::json!([]));
    let content = last.body["content"].as_str().unwrap();
    assert!(content.ends_with(&format!("||{SECRET_FREE_SUMMARY}||")), "{content}");
}

#[tokio::test]
async fn slack_status_uses_a_context_block_in_the_thread() {
    let provider = TestChannelProvider::start().await;
    provider.enqueue_response("chat.postMessage", serde_json::json!({"ok": true, "ts": "171.1"}));
    let surface = SlackStatus {
        http: reqwest::Client::new(),
        token: provider.token.clone(),
        channel: "C1".into(),
        thread_ts: Some("170.0".into()),
    };
    let p = ChannelThinking::new("slack", false, Lang::En, Some(Arc::new(surface)));
    run(&p, ShowThinking::Summary).await;
    let reqs = provider.requests();
    assert_eq!(reqs.iter().filter(|r| r.path.ends_with("chat.postMessage")).count(), 1, "{reqs:?}");
    assert_eq!(reqs[0].body["thread_ts"], "170.0");
    let last = reqs.last().unwrap();
    assert!(last.path.ends_with("chat.update"), "{reqs:?}");
    assert_eq!(last.body["ts"], "171.1");
    assert_eq!(last.body["blocks"][1]["type"], "context");
    assert!(last.body["text"].as_str().unwrap().starts_with("💭 Thought for"));
}
