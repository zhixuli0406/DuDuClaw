use super::*;
use crate::channel_reply::entry_gate_test_helpers::{
    misaligned_worker, assert_no_saved_attachments, assert_refused_before_any_side_effect,
};
use crate::test_channel_provider::TestChannelProvider;

#[tokio::test(flavor = "current_thread")]
async fn bound_employee_refuses_document_and_voice_before_any_download() {
    for media in [
        serde_json::json!({"document":{"file_id":"file-1", "file_name":"report.pdf"}}),
        serde_json::json!({"voice":{"file_id":"voice-1", "duration":1}}),
    ] {
        let provider = TestChannelProvider::start().await;
        let (home, ctx, _runtime, calls) = misaligned_worker("").await;
        let ctx = Arc::new(ctx);
        let before = crate::model_call_probe::calls();
        let mut message = serde_json::json!({
            "message_id":1, "chat":{"id":42,"type":"private"},
            "from":{"id":42,"first_name":"Sender"}
        });
        message.as_object_mut().unwrap().extend(media.as_object().unwrap().clone());
        process_telegram_message(serde_json::from_value(message).unwrap(), reqwest::Client::new(),
            telegram_poll_api_base(&provider.token), provider.token.clone(), ctx.clone(),
            "telegram".into(), Some("worker".into()), "telegram|7".into(), "fixturebot".into()).await;
        assert!(provider.requests().iter().any(|r| r.body["text"].as_str()
            .is_some_and(|text| text.contains("model_provider_mismatch"))));
        assert!(!provider.requests().iter().any(|r| r.path.ends_with("/getFile")));
        assert_no_saved_attachments(home.path());
        assert_refused_before_any_side_effect(&ctx, "telegram:42", &calls, before).await;
    }
}

#[tokio::test(flavor = "current_thread")]
async fn blocked_sender_skips_document_and_voice_without_a_reply_or_saved_file() {
    for agent in [Some("worker"), None] {
        for media in [
            serde_json::json!({"document":{"file_id":"blocked-file", "file_name":"report.pdf"}}),
            serde_json::json!({"voice":{"file_id":"blocked-voice", "duration":1}}),
        ] {
            let provider = TestChannelProvider::start().await;
            let (home, ctx, _runtime, calls) = misaligned_worker("").await;
            ctx.channel_settings.set("telegram", "global", crate::channel_settings::keys::BLOCKED_USERS,
                r#"["42"]"#).await.unwrap();
            let ctx = Arc::new(ctx);
            let before = crate::model_call_probe::calls();
            let mut message = serde_json::json!({
                "message_id":1, "chat":{"id":42,"type":"private"},
                "from":{"id":42,"first_name":"Sender"}, "caption":"檢查附件"
            });
            message.as_object_mut().unwrap().extend(media.as_object().unwrap().clone());
            process_telegram_message(serde_json::from_value(message).unwrap(), reqwest::Client::new(),
                telegram_poll_api_base(&provider.token), provider.token.clone(), ctx.clone(),
                "telegram".into(), agent.map(str::to_string), "telegram|7".into(), "fixturebot".into()).await;
            let requests = provider.requests();
            assert!(!requests.iter().any(|r| r.path.ends_with("/getFile") || r.method == "GET"), "{requests:?}");
            assert!(!requests.iter().any(|r| r.path.ends_with("/sendMessage")), "{requests:?}");
            assert_no_saved_attachments(home.path());
            assert_refused_before_any_side_effect(&ctx, "telegram:42", &calls, before).await;
        }
    }
}
