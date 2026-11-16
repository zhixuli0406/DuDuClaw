use super::*;
use crate::test_channel_provider::TestChannelProvider;
use serde_json::json;

#[tokio::test]
async fn parse_error_retries_plain_and_preserves_url() {
    let provider = TestChannelProvider::start().await;
    provider.enqueue_response("sendMessage", json!({"__status":400,"ok":false,"description":"Bad Request: can't parse entities"}));
    let home = tempfile::tempdir().unwrap();
    send(&reqwest::Client::new(), &crate::telegram::telegram_poll_api_base(&provider.token), "1",
        "https://example.com/a_b", Format::MarkdownHtml, Options::new(home.path(), "notify", "url")).await.unwrap();
    let requests = provider.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].body["parse_mode"], "HTML");
    assert!(requests[1].body.get("parse_mode").is_none());
    assert_eq!(requests[1].body["text"], "https://example.com/a_b");
}

#[test]
fn html_chunks_close_entities_and_preserve_unicode() {
    let input = format!("**{}** & https://example.com/a_b", "貓🐈".repeat(5000));
    let chunks = chunks(&input, Format::MarkdownHtml, None);
    assert!(chunks.len() > 1);
    for chunk in &chunks {
        assert!(chunk.text.len() <= LIMIT);
        assert_eq!(chunk.text.matches("<b>").count(), chunk.text.matches("</b>").count());
        assert!(!chunk.text.ends_with('&'));
    }
    assert!(chunks.iter().map(|c| c.plain.as_str()).collect::<String>().contains(&"貓🐈".repeat(5000)));
}

#[tokio::test]
async fn cron_failure_records_metadata_and_sends_only_content_notice() {
    for status in [400, 401, 403] {
        let provider = TestChannelProvider::start().await;
        let error = json!({"__status":status,"ok":false,"description":if status == 400 {"Bad Request: can't parse entities"} else {"Forbidden"}});
        provider.enqueue_response("sendMessage", error.clone());
        if status == 400 { provider.enqueue_response("sendMessage", error); }
        let home = tempfile::tempdir().unwrap();
        let result = send_cron(&reqwest::Client::new(), &crate::telegram::telegram_poll_api_base(&provider.token),
            "1", "secret result", "daily_report", "cron-186", home.path()).await;
        assert!(result.is_err());
        let requests = provider.requests();
        assert_eq!(requests.len(), if status == 400 {3} else {1});
        if status == 400 {
            assert_eq!(requests[2].body["text"], "⏰ [daily_report] 的結果無法送達，請到儀表板『例行工作』查看");
            assert!(requests[2].body.get("parse_mode").is_none());
        }
        let record = std::fs::read_to_string(home.path().join("channel_failures.jsonl")).unwrap();
        assert!(!record.contains("secret result"));
        let record: Value = serde_json::from_str(record.trim()).unwrap();
        assert_eq!(record["kind"], "cron");
        assert_eq!(record["ref_id"], "cron-186");
        assert_eq!(record["http_status"], status);
    }
}

#[tokio::test]
async fn legacy_second_chunk_falls_back_without_reply_to() {
    let provider = TestChannelProvider::start().await;
    provider.enqueue_response("sendMessage", json!({"ok":true,"result":{"message_id":1}}));
    provider.enqueue_response("sendMessage", json!({"__status":400,"ok":false,"description":"Bad Request: can't parse entities"}));
    let home = tempfile::tempdir().unwrap();
    let input = format!("{}*help*", "a".repeat(LIMIT));
    crate::telegram::send_reply_with_guard(home.path(), &reqwest::Client::new(),
        &crate::telegram::telegram_poll_api_base(&provider.token), 1, &input, None, None, None, None).await.unwrap();
    let requests = provider.requests();
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[1].body["text"], "*help*");
    assert!(requests[2].body.get("parse_mode").is_none());
    assert_eq!(requests[2].body["text"], "*help*");
}

#[tokio::test]
async fn plain_sender_preserves_markdown_characters() {
    let provider = TestChannelProvider::start().await;
    let home = tempfile::tempdir().unwrap();
    assert!(crate::channel_sender::send_plain_text(home.path(), &reqwest::Client::new(), "telegram", &provider.token,
        "1", "a_b * literal https://example.com/a_b").await);
    let requests = provider.requests();
    assert_eq!(requests[0].body["text"], "a_b * literal https://example.com/a_b");
    assert!(requests[0].body.get("parse_mode").is_none());
}

#[test]
fn legacy_entities_stay_whole_and_oversized_entities_become_literal() {
    for entity in ["*bold*", "_italic_", "`code`", "```code```", "[label](https://example.com/a_b)"] {
        let text = format!("{}{entity}", "a".repeat(LIMIT - 1));
        let parts = chunks(&text, Format::LegacyMarkdown, None);
        assert_eq!(parts[1].text, entity);
        assert!(parts.iter().all(|c| c.text.len() <= LIMIT));
    }
    let text = format!("*{}*", "文".repeat(LIMIT));
    let parts = chunks(&text, Format::LegacyMarkdown, None);
    assert!(parts.iter().all(|c| c.mode.is_none()));
    assert_eq!(parts.iter().map(|c| c.text.as_str()).collect::<String>(), text);
}

#[tokio::test]
async fn cron_name_is_literal_and_thread_is_preserved_on_fallback() {
    let provider = TestChannelProvider::start().await;
    let home = tempfile::tempdir().unwrap();
    let mut options = Options::new(home.path(), "cron", "cron-186");
    options.literal_prefix = Some("⏰ [daily_report_*<x>] ");
    options.thread = Some(42);
    provider.enqueue_response("sendMessage", json!({"__status":400,"ok":false,"description":"Bad Request: can't parse entities"}));
    send(&reqwest::Client::new(), &crate::telegram::telegram_poll_api_base(&provider.token), "1",
        "https://example.com/a_b", Format::MarkdownHtml, options).await.unwrap();
    let requests = provider.requests();
    assert!(requests[0].body["text"].as_str().unwrap().starts_with("⏰ [daily_report_*&lt;x&gt;] "));
    assert_eq!(requests[1].body["text"], "⏰ [daily_report_*<x>] https://example.com/a_b");
    assert_eq!(requests[1].body["message_thread_id"], 42);
}

#[test]
fn cron_notice_has_three_languages() {
    assert!(cron_notice("daily_report", Some("en")).contains("Automations"));
    assert!(cron_notice("daily_report", Some("ja-JP")).contains("結果を配信できませんでした"));
    assert!(cron_notice("daily_report", None).contains("例行工作"));
}

#[tokio::test]
async fn visitor_never_gets_operator_notice() {
    let provider = TestChannelProvider::start().await;
    let home = tempfile::tempdir().unwrap();
    for _ in 0..2 {
        provider.enqueue_response("sendMessage", json!({"__status":400,"ok":false,"description":"Bad Request: can't parse entities"}));
    }
    assert!(send_cron(&reqwest::Client::new(), &crate::telegram::telegram_poll_api_base(&provider.token),
        "odoolivechat:visitor", "result", "routine", "cron-186", home.path()).await.is_err());
    assert_eq!(provider.requests().len(), 2);
}
