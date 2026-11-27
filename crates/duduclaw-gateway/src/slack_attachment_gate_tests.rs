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

#[tokio::test(flavor = "current_thread")]
async fn blocked_sender_skips_file_download_and_keeps_the_silent_access_flow() {
    let provider = TestChannelProvider::start().await;
    let (home, ctx, _runtime, calls) = misaligned_worker("").await;
    ctx.channel_settings.set("slack", "global", crate::channel_settings::keys::BLOCKED_USERS,
        r#"["42"]"#).await.unwrap();
    let ctx = Arc::new(ctx);
    let before = crate::model_call_probe::calls();
    // Use the media downloader's existing DNS seam so an accidental GET
    // reaches this fixture instead of being hidden by the SSRF screen.
    let local_url = crate::test_channel_provider::url(&provider.token, "https://slack.com/blocked-file");
    let mut download_url = reqwest::Url::parse(&local_url).unwrap();
    let local: std::net::SocketAddr = format!("{}:{}", download_url.host_str().unwrap(), download_url.port().unwrap()).parse().unwrap();
    let host = format!("blocked-slack-{}.example.net", uuid::Uuid::new_v4());
    crate::web_fetch::test_dns::answer_unvetted(&host, vec![local]);
    download_url.set_host(Some(&host)).unwrap();
    provider.enqueue_response("/blocked-file", serde_json::json!({"__raw":"attachment bytes"}));
    let payload = serde_json::json!({"event":{
        "type":"message", "channel":"D1", "channel_type":"im", "user":"42", "ts":"1700.1",
        "text":"檢查附件", "files":[{"url_private":download_url.as_str(),
            "name":"report.pdf", "mimetype":"application/pdf"}]
    }});
    handle_event(&payload, &provider.token, "BOT", &ctx, &reqwest::Client::new(), Some("worker"), None).await;
    let requests = provider.requests();
    assert!(!requests.iter().any(|r| r.path == "/blocked-file" || r.method == "GET"), "{requests:?}");
    assert!(!requests.iter().any(|r| r.path.ends_with("/chat.postMessage")), "{requests:?}");
    assert_no_saved_attachments(home.path());
    assert_refused_before_any_side_effect(&ctx, "slack:42", &calls, before).await;
}
