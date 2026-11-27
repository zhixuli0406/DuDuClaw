use super::*;
use crate::channel_reply::entry_gate_test_helpers::{
    misaligned_worker, assert_no_saved_attachments, assert_refused_before_any_side_effect,
};
use crate::test_channel_provider::TestChannelProvider;

#[tokio::test(flavor = "current_thread")]
async fn stored_employee_refuses_files_in_the_shared_worker_handler() {
    let provider = TestChannelProvider::start().await;
    let (home, ctx, _runtime, calls) = misaligned_worker("").await;
    let ctx = Arc::new(ctx);
    let before = crate::model_call_probe::calls();
    let state = LineState {
        home_dir: home.path().to_path_buf(), ctx: ctx.clone(), http: reqwest::Client::new(),
        channel_status: ctx.channel_status.clone(), event_tx: ctx.event_tx.clone(), ingress: None,
    };
    let event: LineEvent = serde_json::from_value(serde_json::json!({
        "type":"message", "webhookEventId":"E1", "replyToken":"R1",
        "source":{"type":"user", "userId":"42"},
        "message":{"type":"file", "id":"M1", "fileName":"report.pdf",
            "contentProvider":{"type":"external", "originalContentUrl":"https://example.invalid/report.pdf"}}
    })).unwrap();
    process_line_events(vec![event], &state, &provider.token, "worker", "line-bot-A", None).await;
    assert!(provider.requests().iter().any(|r| r.body["messages"][0]["text"].as_str()
        .is_some_and(|text| text.contains("model_provider_mismatch"))));
    assert_no_saved_attachments(home.path());
    assert_refused_before_any_side_effect(&ctx, "line:42", &calls, before).await;
}
