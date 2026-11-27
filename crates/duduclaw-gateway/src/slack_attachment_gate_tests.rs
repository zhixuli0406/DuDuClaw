use super::*;
use crate::channel_reply::entry_gate_test_helpers::{
    misaligned_worker, assert_no_saved_attachments, assert_refused_before_any_side_effect,
};
use crate::test_channel_provider::TestChannelProvider;

#[tokio::test(flavor = "current_thread")]
async fn own_bot_refuses_files_before_reactions_and_downloads() {
    let provider = TestChannelProvider::start().await;
    let (home, ctx, _runtime, calls) = misaligned_worker("").await;
    let ctx = Arc::new(ctx);
    let before = crate::model_call_probe::calls();
    let payload = serde_json::json!({"event":{
        "type":"message", "channel":"D1", "channel_type":"im", "user":"42", "ts":"1700.1",
        "text":"檢查附件", "files":[{"url_private":"https://example.invalid/report.pdf",
            "name":"report.pdf", "mimetype":"application/pdf"}]
    }});
    handle_event(&payload, &provider.token, "BOT", &ctx, &reqwest::Client::new(), Some("worker"), None).await;
    assert!(provider.requests().iter().any(|r| r.body["text"].as_str()
        .is_some_and(|text| text.contains("model_provider_mismatch"))));
    assert!(!provider.requests().iter().any(|r| r.path.ends_with("/reactions.add")));
    assert_no_saved_attachments(home.path());
    assert_refused_before_any_side_effect(&ctx, "slack:42", &calls, before).await;
}
