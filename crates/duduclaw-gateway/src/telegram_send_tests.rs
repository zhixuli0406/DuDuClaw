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
            assert_eq!(requests[2].body["text"], "⏰ [daily_report] 的結果無法送達，請到儀表板「自動化」查看");
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
    assert!(cron_notice("daily_report", None).contains("自動化"));
    for incomplete in [false, true] {
        assert!(delivery_notice(Some("en-US"), incomplete).contains("Automations"));
        assert!(delivery_notice(Some("ja-JP"), incomplete).contains("「自動化」"));
        assert!(delivery_notice(None, incomplete).contains("「自動化」"));
    }
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

#[tokio::test]
async fn telegram_delivery_caps_at_fifteen_chunks_and_plain_notice() {
    for format in [Format::Plain, Format::MarkdownHtml, Format::LegacyMarkdown] {
        let provider = TestChannelProvider::start().await;
        let home = tempfile::tempdir().unwrap();
        send(&reqwest::Client::new(), &crate::telegram::telegram_poll_api_base(&provider.token), "1",
            &"a".repeat(LIMIT * 20), format, Options::new(home.path(), "notify", "cap")).await.unwrap();
        let requests = provider.requests();
        assert_eq!(requests.len(), 16);
        assert!(requests.iter().take(15).all(|r| r.body["text"] == "a".repeat(LIMIT)));
        assert_eq!(requests[15].body["text"], delivery_notice(None, false));
        assert!(requests[15].body.get("parse_mode").is_none());
    }
}

#[test]
fn retry_after_is_clamped() {
    for (requested, expected) in [(0, 1), (1, 1), (30, 30), (31, 30), (u64::MAX, 30)] {
        assert_eq!(retry_seconds(&json!({"parameters":{"retry_after":requested}}), 1), expected);
    }
    assert_eq!(retry_seconds(&Value::Null, 2), 3);
}

#[tokio::test]
async fn non_json_502_and_telegram_5xx_are_retried() {
    for raw in ["", "upstream unavailable"] {
        let provider = TestChannelProvider::start().await;
        provider.enqueue_response("sendMessage", json!({"__status":502,"__raw":raw}));
        provider.enqueue_response("sendMessage", json!({"ok":false,"error_code":503}));
        let home = tempfile::tempdir().unwrap();
        send(&reqwest::Client::new(), &crate::telegram::telegram_poll_api_base(&provider.token), "1",
            "test", Format::Plain, Options::new(home.path(), "notify", "retry")).await.unwrap();
        assert_eq!(provider.requests().len(), 3);
    }
}

#[tokio::test]
async fn fallback_combinations_cover_entity_then_missing_reply_and_reverse() {
    for errors in [vec!["can't parse entities", "message to be replied not found"],
        vec!["message to be replied not found", "can't parse entities"]] {
        let provider = TestChannelProvider::start().await;
        for description in &errors {
            provider.enqueue_response("sendMessage", json!({"__status":400,"ok":false,"description":description}));
        }
        let home = tempfile::tempdir().unwrap();
        let mut options = Options::new(home.path(), "notify", "fallback");
        options.reply_to = Some(42);
        send(&reqwest::Client::new(), &crate::telegram::telegram_poll_api_base(&provider.token), "1",
            "**hello**", Format::MarkdownHtml, options).await.unwrap();
        let requests = provider.requests();
        assert_eq!(requests.len(), 3);
        assert_eq!(requests[0].body["parse_mode"], "HTML");
        assert!(requests[0].body.get("reply_parameters").is_some());
        if errors[0] == "can't parse entities" {
            assert!(requests[1].body.get("parse_mode").is_none());
            assert!(requests[1].body.get("reply_parameters").is_some());
        } else {
            assert_eq!(requests[1].body["parse_mode"], "HTML");
            assert!(requests[1].body.get("reply_parameters").is_none());
        }
        assert!(requests[2].body.get("parse_mode").is_none());
        assert!(requests[2].body.get("reply_parameters").is_none());
    }
}

#[tokio::test]
async fn markdown_link_plain_fallback_keeps_target_and_chunk_alignment() {
    let provider = TestChannelProvider::start().await;
    provider.enqueue_response("sendMessage", json!({"__status":400,"ok":false,"description":"can't parse entities"}));
    let home = tempfile::tempdir().unwrap();
    send(&reqwest::Client::new(), &crate::telegram::telegram_poll_api_base(&provider.token), "1",
        "[help](https://example.com/help)", Format::MarkdownHtml, Options::new(home.path(), "notify", "link")).await.unwrap();
    assert_eq!(provider.requests()[1].body["text"], "help (https://example.com/help)");
    let input = format!("{}[help](https://example.com/help) rest", "a".repeat(LIMIT - 35));
    let parts = chunks(&input, Format::MarkdownHtml, None);
    assert!(parts.iter().all(|c| c.text.len() <= LIMIT && c.plain.len() <= LIMIT));
    assert_eq!(parts.iter().map(|c| c.plain.as_str()).collect::<String>(),
        format!("{}help (https://example.com/help) rest", "a".repeat(LIMIT - 35)));
    let same = chunks("[https://example.com](https://example.com)", Format::MarkdownHtml, None);
    assert_eq!(same[0].plain, "https://example.com");
}

#[tokio::test]
async fn otp_brand_is_delivered_literally_without_parse_mode() {
    use crate::channel_sender::ChannelSender;
    let provider = TestChannelProvider::start().await;
    let home = tempfile::tempdir().unwrap();
    let sender = crate::channel_sender::TelegramSender {
        home_dir: home.path().into(), bot_token: provider.token.clone(), chat_id: "1".into(), http: reqwest::Client::new(),
    };
    let text = crate::otp_delivery::message("telegram", "A&B_* @brand", "123456");
    sender.send_text_context(&text, "otp", "1").await.unwrap();
    assert_eq!(provider.requests()[0].body["text"], "A&B_* @\u{200b}brand 登入驗證碼：123456\n5 分鐘內有效，請勿分享給任何人。");
    assert!(provider.requests()[0].body.get("parse_mode").is_none());
}

#[tokio::test]
async fn delivery_budget_cancels_a_later_request_and_records_non_content_failure() {
    let provider = TestChannelProvider::start().await;
    for delay in [40000, 40000, 60000] {
        provider.enqueue_response("sendMessage", json!({"__delay_millis":delay,"ok":true,"result":{}}));
    }
    let home = tempfile::tempdir().unwrap();
    let client = reqwest::Client::new();
    let base = crate::telegram::telegram_poll_api_base(&provider.token);
    let text = "a".repeat(LIMIT * 4);
    let delivery = send(&client, &base, "1", &text, Format::Plain, Options::new(home.path(), "notify", "budget"));
    let clock = async {
        for count in 1..=3 {
            while provider.requests().len() < count { tokio::task::yield_now().await; }
            if count == 1 { tokio::time::pause(); }
            tokio::time::advance(Duration::from_secs(40)).await;
        }
    };
    let (result, ()) = tokio::join!(delivery, clock);
    tokio::time::resume();
    let error = result.unwrap_err();
    assert!(!error.content);
    assert_eq!(error.description, "Telegram delivery budget exhausted");
    assert_eq!(error.delivered_chunks, 2);
    assert_eq!(provider.requests().len(), 3);
    let record = std::fs::read_to_string(home.path().join("channel_failures.jsonl")).unwrap();
    assert_eq!(serde_json::from_str::<Value>(record.trim()).unwrap()["failure_kind"], "transport_or_access");
}

#[tokio::test]
async fn request_attempt_times_out_after_45_seconds() {
    let provider = TestChannelProvider::start().await;
    provider.enqueue_response("sendMessage", json!({"__delay_millis":60000,"ok":true}));
    let home = tempfile::tempdir().unwrap();
    let client = reqwest::Client::new();
    let base = crate::telegram::telegram_poll_api_base(&provider.token);
    let delivery = send(&client, &base, "1", "test", Format::Plain, Options::new(home.path(), "notify", "timeout"));
    let clock = async {
        while provider.requests().is_empty() { tokio::task::yield_now().await; }
        tokio::time::pause();
        tokio::time::advance(Duration::from_secs(46)).await;
    };
    let (result, ()) = tokio::join!(delivery, clock);
    tokio::time::resume();
    let error = result.unwrap_err();
    assert_eq!(error.description, "Telegram request timeout");
    assert!(!error.content);
}

#[tokio::test]
async fn partial_delivery_tracks_progress_and_sends_one_incomplete_notice() {
    let provider = TestChannelProvider::start().await;
    provider.enqueue_response("sendMessage", json!({"ok":true,"result":{}}));
    provider.enqueue_response("sendMessage", json!({"__status":403,"ok":false,"description":"Forbidden"}));
    let home = tempfile::tempdir().unwrap();
    let (result, partial) = track_partial(send(&reqwest::Client::new(),
        &crate::telegram::telegram_poll_api_base(&provider.token), "1", &"a".repeat(LIMIT + 1),
        Format::Plain, Options::new(home.path(), "notify", "partial"))).await;
    assert!(partial);
    assert_eq!(result.unwrap_err().delivered_chunks, 1);
    let requests = provider.requests();
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[2].body["text"], delivery_notice(None, true));
    assert!(requests[2].body.get("parse_mode").is_none());
    let record = std::fs::read_to_string(home.path().join("channel_failures.jsonl")).unwrap();
    assert_eq!(serde_json::from_str::<Value>(record.trim()).unwrap()["delivered_chunks"], 1);
}

#[tokio::test]
async fn oversized_markdown_is_bounded_before_rendering() {
    let provider = TestChannelProvider::start().await;
    let home = tempfile::tempdir().unwrap();
    // The complete link must never reach the renderer: its closing delimiter is
    // several MiB beyond the input budget, including a multibyte boundary.
    let input = format!("[help]({})", "貓🐈".repeat(500_000));
    let bounded = duduclaw_core::truncate_bytes(&input, MAX_INPUT_BYTES);
    assert!(bounded.len() < MAX_INPUT_BYTES);
    let expected = chunks(bounded, Format::MarkdownHtml, None);
    send(&reqwest::Client::new(), &crate::telegram::telegram_poll_api_base(&provider.token), "1",
        &input, Format::MarkdownHtml, Options::new(home.path(), "notify", "oversized")).await.unwrap();
    let requests = provider.requests();
    assert!(requests.len() <= 16);
    assert_eq!(requests[0].body["text"], expected[0].text);
    assert!(requests[0].body["text"].as_str().unwrap().starts_with("[help]("));
    assert_eq!(requests.last().unwrap().body["text"], delivery_notice(None, false));
    assert!(requests.last().unwrap().body.get("parse_mode").is_none());
}

#[tokio::test]
async fn oversized_literal_prefix_and_text_share_rendering_budget() {
    let provider = TestChannelProvider::start().await;
    let home = tempfile::tempdir().unwrap();
    let prefix = "p".repeat(3 * 1024 * 1024);
    let text = "t".repeat(3 * 1024 * 1024);
    let mut options = Options::new(home.path(), "notify", "oversized-prefix");
    options.literal_prefix = Some(&prefix);
    send(&reqwest::Client::new(), &crate::telegram::telegram_poll_api_base(&provider.token), "1",
        &text, Format::MarkdownHtml, options).await.unwrap();
    let requests = provider.requests();
    assert!(requests.len() <= 16);
    let rendered_bytes: usize = requests.iter()
        .map(|request| request.body["text"].as_str().unwrap().len()).sum();
    assert!(rendered_bytes <= 16 * LIMIT + delivery_notice(None, false).len());
    assert_eq!(requests.last().unwrap().body["text"], delivery_notice(None, false));
}

#[tokio::test]
async fn truncated_markdown_with_few_rendered_chunks_still_gets_fixed_notice() {
    let provider = TestChannelProvider::start().await;
    let home = tempfile::tempdir().unwrap();
    let input = format!("**hello**\n\n{}", "\n".repeat(3 * 1024 * 1024));
    send(&reqwest::Client::new(), &crate::telegram::telegram_poll_api_base(&provider.token), "1",
        &input, Format::MarkdownHtml, Options::new(home.path(), "notify", "short-render")).await.unwrap();
    let requests = provider.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].body["text"], "<b>hello</b>");
    assert_eq!(requests[1].body["text"], delivery_notice(None, false));
    assert!(requests[1].body.get("parse_mode").is_none());
}

#[tokio::test]
async fn cron_body_and_failure_notice_share_one_delivery_budget() {
    let provider = TestChannelProvider::start().await;
    for delay in [40000, 40000] {
        provider.enqueue_response("sendMessage", json!({"__delay_millis":delay,"ok":true,"result":{}}));
    }
    provider.enqueue_response("sendMessage", json!({"__delay_millis":30000,"__status":400,"ok":false,"description":"message is too long"}));
    provider.enqueue_response("sendMessage", json!({"__delay_millis":40000,"ok":true,"result":{}}));
    let home = tempfile::tempdir().unwrap();
    let client = reqwest::Client::new();
    let base = crate::telegram::telegram_poll_api_base(&provider.token);
    let input = "a".repeat(LIMIT * 3);
    let delivery = send_cron(&client, &base, "1", &input, "budget", "cron-budget", home.path());
    let clock = async {
        let mut started = None;
        for (index, seconds) in [40, 40, 30, 10].into_iter().enumerate() {
            while provider.requests().len() <= index { tokio::task::yield_now().await; }
            if index == 0 {
                tokio::time::pause();
                started = Some(Instant::now());
            }
            tokio::time::advance(Duration::from_secs(seconds)).await;
        }
        started.unwrap()
    };
    let (result, started) = tokio::join!(delivery, clock);
    let elapsed = started.elapsed();
    tokio::time::resume();
    assert!(result.unwrap_err().content);
    assert_eq!(provider.requests().len(), 4);
    assert!(elapsed <= Duration::from_secs(120), "elapsed: {elapsed:?}");
    assert!(provider.requests()[3].body["text"].as_str().unwrap().contains("結果無法送達"));
}

#[tokio::test]
async fn partial_visitor_delivery_sets_flag_without_incomplete_notice() {
    let provider = TestChannelProvider::start().await;
    provider.enqueue_response("sendMessage", json!({"ok":true,"result":{}}));
    provider.enqueue_response("sendMessage", json!({"__status":403,"ok":false,"description":"Forbidden"}));
    let home = tempfile::tempdir().unwrap();
    let (result, partial) = track_partial(send(&reqwest::Client::new(),
        &crate::telegram::telegram_poll_api_base(&provider.token), "odoolivechat:visitor", &"a".repeat(LIMIT + 1),
        Format::Plain, Options::new(home.path(), "notify", "visitor-partial"))).await;
    assert!(partial);
    assert_eq!(result.unwrap_err().delivered_chunks, 1);
    assert_eq!(provider.requests().len(), 2);
}
