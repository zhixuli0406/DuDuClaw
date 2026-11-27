use super::*;
use crate::channel_reply::entry_gate_test_helpers::{
    misaligned_worker, assert_no_saved_attachments, assert_refused_before_any_side_effect,
};
use crate::agent_channels::e2e_gchat_tests as chat;

#[tokio::test(flavor = "current_thread")]
async fn own_app_refuses_media_before_download_or_placeholder() {
    let fake = chat::start_fake().await;
    let (home, ctx, _runtime, calls) = misaligned_worker("").await;
    let ctx = Arc::new(ctx);
    let before = crate::model_call_probe::calls();
    let raw = chat::service_account(&fake, &format!("attachment-{}@test.invalid", uuid::Uuid::new_v4()));
    let state = Arc::new(GoogleChatState {
        ctx: ctx.clone(), creds: Arc::new(GoogleChatCreds::candidate(&raw).unwrap()),
        agent: Some("worker".into()),
    });
    let mut event = chat::message_event("spaces/AAA", "檢查附件");
    event["message"]["attachment"] = serde_json::json!([{
        "name":"spaces/AAA/messages/BBB/attachments/CCC", "contentName":"report.pdf",
        "contentType":"application/pdf", "source":"UPLOADED_CONTENT",
        "attachmentDataRef":{"resourceName":"spaces/AAA/messages/BBB/attachments/CCC"}
    }]);
    let conversation = crate::googlechat_conversation::GoogleChatConversation::from_event(&event, false, true).unwrap();
    handle_message(&state, &event, &conversation, true).await;
    assert!(chat::chat_calls(&fake).iter().any(|(_, _, _, body)| body["text"].as_str()
        .is_some_and(|text| text.contains("model_provider_mismatch"))));
    assert!(!chat::chat_queries(&fake).iter().any(|(path, query)|
        path.contains("/media/") || query.split('&').any(|pair| pair.starts_with("messageId="))));
    assert_no_saved_attachments(home.path());
    assert_refused_before_any_side_effect(&ctx, conversation.session_key(), &calls, before).await;
}
