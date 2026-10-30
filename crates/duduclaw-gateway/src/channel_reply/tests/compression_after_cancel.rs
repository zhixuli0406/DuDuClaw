//! Capture the real prompt builder immediately before its runtime work.
use super::*;

tokio::task_local! {
    pub(crate) static CAPTURE_PROMPT: ();
}

pub(crate) fn capture(prompt: &str, history: &[ConversationTurn]) -> Option<String> {
    CAPTURE_PROMPT.try_with(|_| {
        serde_json::json!({
            "prompt": prompt,
            "history": history.iter().map(|turn| serde_json::json!({
                "role": turn.role, "content": turn.content,
            })).collect::<Vec<_>>(),
        }).to_string()
    }).ok()
}

#[tokio::test]
async fn webchat_cancel_preserves_real_compression_in_next_turn_prompt() {
    let home = tempfile::tempdir().unwrap();
    crate::trigger_route::tests::write_agent(
        &home.path().join("agents"), "dudu", "Main", "@dudu", "main", "",
    );
    let ctx = crate::agent_channels::e2e_tests::reply_ctx(home.path()).await;
    let sessions = &ctx.session_manager;
    let sid = "webchat:summary-regression";
    sessions.get_or_create(sid, "dudu").await.unwrap();
    sessions.compress(sid, "REAL_COMPRESSION: the chosen color is blue").await.unwrap();
    sessions.append_message(sid, "user", "Previous ordinary user message", 5).await.unwrap();
    sessions.record_webchat_cancel(sid, "dudu", "cancelled-turn-marker", "cancelled-msg-marker")
        .await.unwrap();
    // Legacy system control rows must not supersede a real compressed summary.
    sessions.append_message(sid, "system", r#"{"status":"cancelled","turn_id":"legacy-cancel-marker"}"#, 0)
        .await.unwrap();
    let actual = CAPTURE_PROMPT.scope((), Box::pin(build_reply_with_session_inner(
        "Hello", &ctx, Some("dudu"), sid, "test-user", None,
    ))).await;
    let captured: serde_json::Value = serde_json::from_str(&actual)
        .unwrap_or_else(|error| panic!("prompt boundary was not reached: {error}; reply={actual}"));
    let prompt = captured["prompt"].as_str().unwrap();
    assert!(prompt.contains("## Prior Conversation Summary\nREAL_COMPRESSION: the chosen color is blue"), "{prompt}");
    assert!(!actual.contains("cancelled-turn-marker"));
    assert!(!actual.contains("cancelled-msg-marker"));
    assert!(!actual.contains("legacy-cancel-marker"));
    assert!(captured["history"].as_array().unwrap().iter().all(|turn| turn["role"] != "system"));
    assert!(captured["history"].to_string().contains("Previous ordinary user message"));
}
