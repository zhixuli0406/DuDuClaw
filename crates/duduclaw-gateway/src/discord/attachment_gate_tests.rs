use super::*;
use crate::channel_reply::entry_gate_test_helpers::{
    misaligned_worker, assert_no_saved_attachments, assert_refused_before_any_side_effect,
};
use crate::test_channel_provider::TestChannelProvider;

fn message(text: &str) -> Value {
    json!({
        "id": "M1", "channel_id": "C1",
        "author": { "id": "42", "username": "Sender", "bot": false },
        "content": text,
        "attachments": [{ "url": "https://example.invalid/report.pdf",
            "filename": "report.pdf", "content_type": "application/pdf" }]
    })
}

#[tokio::test(flavor = "current_thread")]
async fn broken_bot_employee_decline_with_attachment_reaches_decision_fastlane() {
    use crate::channel_decision_route::test_support::{provider_sent_text, status_of};
    use crate::decision_notify::native_loop_fixture::{NativeCuFixture, pending_native_request};

    let provider = TestChannelProvider::start().await;
    let fixture = NativeCuFixture::new(&provider.token).await;
    let id = pending_native_request(&fixture, crate::approval::DecisionContext {
        channel: "discord".into(), account_id: "A1".into(),
        conversation_id: "C1".into(), principal_id: "42".into(),
    }).await;
    let path = fixture.home.path().join("agents/alice/agent.toml");
    let policy = crate::approval::policy_revision(fixture.home.path(), "alice").unwrap();
    let raw = std::fs::read_to_string(&path).unwrap();
    // Keep the approval's authority binding current while breaking model admission.
    std::fs::write(path, format!("{raw}\n[runtime]\nprovider='codex'\n[model]\npreferred='claude-opus-4-6'\n")).unwrap();
    assert_eq!(crate::approval::policy_revision(fixture.home.path(), "alice").unwrap(), policy);
    assert_eq!(crate::entry_gate::check_task_assignee(fixture.home.path(), "alice").unwrap_err().code(),
        "model_provider_mismatch");
    assert!(agent_selection::explicit_agent(&fixture.ctx, Some("alice"), "").await.is_err());
    let records = ordinary_dispatch_probe::install(&provider.token);

    handle_message_create(&message(&format!("<@BOT> deny {}", id.as_str())), "BOT",
        &reqwest::Client::new(), &provider.token, &fixture.ctx, Some("alice"), "A1").await;

    assert_eq!(status_of(fixture.home.path(), &id).await, crate::approval::ApprovalStatus::Denied);
    assert!(!provider_sent_text(&provider, crate::channel_reply::SELECTED_AGENT_UNAVAILABLE));
    assert_eq!(attachment_download_probe::count(&provider.token), 0);
    assert_no_saved_attachments(fixture.home.path());
    assert!(!fixture.home.path().join("agents/alice/attachments").exists());
    assert!(records.lock().unwrap().is_empty());
    ordinary_dispatch_probe::remove(&provider.token);
}

#[tokio::test(flavor = "current_thread")]
async fn unavailable_bot_command_with_files_preserves_thread_guidance() {
    let provider = TestChannelProvider::start().await;
    let (home, ctx, _runtime, calls) = misaligned_worker("").await;
    let ctx = Arc::new(ctx);
    let before = crate::model_call_probe::calls();
    provider.enqueue_response("/channels/C1", json!({"id":"C1", "type":0, "guild_id":"G1"}));
    let mut data = message("<@BOT> /new");
    data["guild_id"] = json!("G1");
    data["mentions"] = json!([{ "id": "BOT" }]);

    handle_message_create(&data, "BOT", &reqwest::Client::new(),
        &provider.token, &ctx, Some("missing-worker"), "A1").await;

    assert!(provider.requests().iter().any(|r| r.body["content"].as_str()
        == Some("此頻道的對話都在各自的對話串內進行，請在對話串內使用此指令。")));
    assert!(!crate::channel_decision_route::test_support::provider_sent_text(
        &provider, crate::channel_reply::SELECTED_AGENT_UNAVAILABLE));
    assert_eq!(attachment_download_probe::count(&provider.token), 0);
    assert_no_saved_attachments(home.path());
    assert_refused_before_any_side_effect(&ctx, "discord:C1", &calls, before).await;
}

#[tokio::test(flavor = "current_thread")]
async fn refused_message_never_downloads_writes_or_enters_work() {
    for malformed in [false, true] {
        let provider = TestChannelProvider::start().await;
        let (home, ctx, _runtime, calls) = misaligned_worker("").await;
        if malformed {
            std::fs::write(home.path().join("agents/worker/agent.toml"), "[model").unwrap();
        }
        let ctx = Arc::new(ctx);
        let before = crate::model_call_probe::calls();
        handle_message_create(&message("檢查附件"), "BOT", &reqwest::Client::new(),
            &provider.token, &ctx, Some("worker"), "A1").await;
        let code = if malformed { "agent_config_unreadable" } else { "model_provider_mismatch" };
        assert!(provider.requests().iter().any(|r|
            r.body["content"].as_str().is_some_and(|text| text.contains(code))));
        assert_eq!(attachment_download_probe::count(&provider.token), 0);
        assert_no_saved_attachments(home.path());
        assert_refused_before_any_side_effect(&ctx, "discord:C1", &calls, before).await;
    }
}

#[tokio::test(flavor = "current_thread")]
async fn broken_employee_drops_files_but_help_still_runs() {
    let provider = TestChannelProvider::start().await;
    let (home, ctx, _runtime, calls) = misaligned_worker("").await;
    let ctx = Arc::new(ctx);
    let before = crate::model_call_probe::calls();
    handle_message_create(&message("<@BOT> /help"), "BOT", &reqwest::Client::new(),
        &provider.token, &ctx, Some("worker"), "A1").await;
    assert!(provider.requests().iter().any(|r|
        r.body["content"].as_str().is_some_and(|text| !text.is_empty()
            && !text.contains("model_provider_mismatch"))));
    assert_eq!(attachment_download_probe::count(&provider.token), 0);
    assert_no_saved_attachments(home.path());
    assert_refused_before_any_side_effect(&ctx, "discord:C1", &calls, before).await;
}

#[tokio::test(flavor = "current_thread")]
async fn new_thread_binding_is_checked_instead_of_the_parent_fallback() {
    let provider = TestChannelProvider::start().await;
    let (home, ctx, _runtime, calls) = misaligned_worker(
        "\n[[channels.discord.bindings]]\nkind='thread'\nid='T1'\n",
    ).await;
    let ctx = Arc::new(ctx);
    let before = crate::model_call_probe::calls();
    provider.enqueue_response("/channels/C1", json!({"id":"C1", "type":0, "guild_id":"G1"}));
    provider.enqueue_response("/channels/C1/messages/M1/threads", json!({"id":"T1"}));
    let mut data = message("檢查附件");
    data["guild_id"] = json!("G1");
    handle_message_create(&data, "BOT", &reqwest::Client::new(),
        &provider.token, &ctx, None, "A1").await;
    assert!(provider.requests().iter().any(|r| r.path.ends_with("/channels/T1/messages")
        && r.body["content"].as_str().is_some_and(|text| text.contains("model_provider_mismatch"))));
    assert_eq!(attachment_download_probe::count(&provider.token), 0);
    assert_no_saved_attachments(home.path());
    assert_refused_before_any_side_effect(&ctx, "discord:thread:T1", &calls, before).await;
}
