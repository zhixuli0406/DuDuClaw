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
    let summary_context = crate::session_summarizer::summary_system_context(
        "REAL_COMPRESSION: the chosen color is blue",
    );
    assert!(prompt.contains(&summary_context), "{prompt}");
    assert!(!prompt.contains("## Prior Conversation Summary"), "{prompt}");
    assert!(!actual.contains("cancelled-turn-marker"));
    assert!(!actual.contains("cancelled-msg-marker"));
    assert!(!actual.contains("legacy-cancel-marker"));
    assert!(captured["history"].as_array().unwrap().iter().all(|turn| turn["role"] != "system"));
    assert!(captured["history"].to_string().contains("Previous ordinary user message"));
}


#[tokio::test]
async fn merged_webchat_dispatch_keeps_original_messages_as_individual_history_rows() {
    let home = tempfile::tempdir().unwrap();
    crate::trigger_route::tests::write_agent(
        &home.path().join("agents"), "dudu", "Main", "@dudu", "main", "",
    );
    let ctx = crate::agent_channels::e2e_tests::reply_ctx(home.path()).await;
    let sid = "webchat:merged-history";
    ctx.session_manager.get_or_create(sid, "dudu").await.unwrap();
    let originals = vec![("first".to_string(), "第一個補充".to_string()),
        ("second".to_string(), "第二個補充😀".to_string())];
    let merged = "第一個補充\n\n---\n第二個補充😀";
    let actual = WEBCHAT_ORIGINAL_MESSAGES.scope(originals.clone(), CAPTURE_PROMPT.scope((),
        Box::pin(build_reply_with_session_inner(merged, &ctx, Some("dudu"), sid, "test-user", None)),
    )).await;
    let captured: serde_json::Value = serde_json::from_str(&actual).expect("must reach the prompt boundary");
    assert!(captured.to_string().contains("第一個補充"));
    let rows = ctx.session_manager.get_messages(sid).await.unwrap();
    let users: Vec<_> = rows.iter().filter(|row| row.role == "user").collect();
    assert_eq!(users.len(), 2, "the merged dispatch must not create a combined history row");
    for (row, (_, original)) in users.iter().zip(&originals) {
        assert!(row.content.ends_with(original));
        assert!(!row.content.contains("\n\n---\n"));
    }
    let conn = ctx.session_manager.acquire().await;
    let mut query = conn.prepare("SELECT client_msg_id FROM session_messages WHERE session_id=?1 AND role='user' ORDER BY id").unwrap();
    let ids: Vec<String> = query.query_map([sid], |row| row.get(0)).unwrap().map(Result::unwrap).collect();
    assert_eq!(ids, vec!["first", "second"]);
}
