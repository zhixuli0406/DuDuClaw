//! Tool-loop tests (cases_provenance), moved verbatim out of `tool_loop.rs`.

use super::*;

#[tokio::test]
async fn ccr_outcome_omits_handle_revoked_by_replayed_call() {
    let dir = tempfile::tempdir().unwrap();
    let store = crate::ccr::CcrStore::new(dir.path().join("ccr.db"));
    let provider = ScriptedProvider::new(vec![
        tool_use_resp("same-call", "search"),
        tool_use_resp("same-call", "search"),
        final_resp("done"),
    ]);
    let exec = MockExecutor::new(MockBehavior::Sequence(vec![
        "first original\n".repeat(500),
        "different original\n".repeat(500),
    ]));
    let runtime = CcrRuntime::new_unrestricted_for_test(store, ccr_test_scope());
    let outcome = run_tool_loop_with_provenance_and_ccr(
        &provider,
        ChatRequest::new("m"),
        &exec,
        3,
        ProvenanceConfig::default(),
        None,
        Some(runtime.clone()),
    )
    .await
    .unwrap();
    assert_eq!(exec.call_count(), 2);
    assert!(outcome.ccr_saved_results.is_empty());
    assert!(runtime.find("first original", 5).unwrap().is_empty());
}

#[tokio::test]
async fn ccr_changed_short_result_revokes_prior_large_call_handle() {
    let dir = tempfile::tempdir().unwrap();
    let store = crate::ccr::CcrStore::new(dir.path().join("ccr.db"));
    let previous = store
        .put(
            &ccr_test_scope(),
            "search",
            "call-1",
            &"old value ".repeat(700),
        )
        .unwrap();
    let provider =
        ScriptedProvider::new(vec![tool_use_resp("call-1", "search"), final_resp("done")]);
    let exec = MockExecutor::new(MockBehavior::Ok("changed short value".into()));
    run_tool_loop_with_provenance_and_ccr(
        &provider,
        ChatRequest::new("m"),
        &exec,
        3,
        ProvenanceConfig::default(),
        None,
        Some(CcrRuntime::new_unrestricted_for_test(
            store.clone(),
            ccr_test_scope(),
        )),
    )
    .await
    .unwrap();
    let sent = provider.last_request();
    let ContentPart::ToolResult {
        content, is_error, ..
    } = &sent.messages.last().unwrap().parts[0]
    else {
        panic!("missing tool result")
    };
    assert!(*is_error);
    assert!(content.contains("revoked"));
    assert!(!content.contains("changed short value"));
    assert!(matches!(
        store.retrieve(&ccr_test_scope(), &previous.id, None, 0, 64),
        Err(crate::ccr::CcrError::NotFound)
    ));
}

#[tokio::test]
async fn ccr_revocation_withholds_short_success_and_error_results() {
    let dir = tempfile::tempdir().unwrap();
    let store = crate::ccr::CcrStore::new(dir.path().join("ccr.db"));
    store
        .revoke_source_call(&ccr_test_scope(), "search", "call-1")
        .unwrap();
    for behavior in [
        MockBehavior::Ok("short secret".into()),
        MockBehavior::Error("short secret".into()),
        MockBehavior::Dispatch("short secret".into()),
    ] {
        let provider =
            ScriptedProvider::new(vec![tool_use_resp("call-1", "search"), final_resp("done")]);
        let exec = MockExecutor::new(behavior);
        run_tool_loop_with_provenance_and_ccr(
            &provider,
            ChatRequest::new("m"),
            &exec,
            3,
            ProvenanceConfig::default(),
            None,
            Some(CcrRuntime::new_unrestricted_for_test(
                store.clone(),
                ccr_test_scope(),
            )),
        )
        .await
        .unwrap();
        let sent = provider.last_request();
        let ContentPart::ToolResult {
            content, is_error, ..
        } = &sent.messages.last().unwrap().parts[0]
        else {
            panic!("missing tool result")
        };
        assert!(*is_error);
        assert!(content.contains("revoked"));
        assert!(!content.contains("short secret"));
    }
}

#[tokio::test]
async fn dispatches_tool_then_terminates_on_end_turn() {
    let provider = ScriptedProvider::new(vec![
        tool_use_resp("call-1", "search"),
        final_resp("here is the answer"),
    ]);
    let exec = MockExecutor::new(MockBehavior::Ok("result payload".into()));
    let req = ChatRequest::new("anthropic/claude-haiku-4-5");

    let resp = run_tool_loop(&provider, req, &exec, DEFAULT_MAX_TOOL_ITERS)
        .await
        .unwrap();

    assert_eq!(resp.text(), "here is the answer");
    assert_eq!(resp.stop, StopReason::EndTurn);
    assert_eq!(exec.call_count(), 1);
    // Provider called twice: initial + after tool result.
    assert_eq!(provider.calls(), 2);

    // The last request must carry the echoed assistant tool-call turn plus
    // a matching User tool-result turn.
    let last = provider.last_request();
    assert_eq!(last.messages.len(), 2);
    assert_eq!(last.messages[0].role, Role::Assistant);
    assert!(matches!(
        last.messages[0].parts[0],
        ContentPart::ToolCall { .. }
    ));
    assert_eq!(last.messages[1].role, Role::User);
    match &last.messages[1].parts[0] {
        ContentPart::ToolResult {
            call_id,
            content,
            is_error,
        } => {
            assert_eq!(call_id, "call-1");
            assert_eq!(content, "result payload");
            assert!(!is_error);
        }
        other => panic!("expected ToolResult, got {other:?}"),
    }
}

#[tokio::test]
async fn seeds_tools_from_executor_when_request_has_none() {
    let provider = ScriptedProvider::new(vec![final_resp("done")]);
    let exec = MockExecutor::new(MockBehavior::Ok("x".into()));
    let req = ChatRequest::new("m");
    assert!(req.tools.is_empty());

    run_tool_loop(&provider, req, &exec, 5).await.unwrap();

    let seen = provider.last_request();
    assert_eq!(seen.tools.len(), 1);
    assert_eq!(seen.tools[0].name, "search");
}

#[tokio::test]
async fn max_iters_exhaustion_returns_marker() {
    // Provider always asks for a tool → loop can never end naturally.
    let provider = ScriptedProvider::new(vec![tool_use_resp("c", "search")]);
    let exec = MockExecutor::new(MockBehavior::Ok("again".into()));
    let req = ChatRequest::new("m");

    let resp = run_tool_loop(&provider, req, &exec, 2).await.unwrap();

    assert_eq!(resp.stop, StopReason::Other(MAX_ITERS_STOP.into()));
    // Exactly `max_iters` dispatch rounds executed.
    assert_eq!(exec.call_count(), 2);
    // Provider called 1 (initial) + 2 (per round) = 3 times.
    assert_eq!(provider.calls(), 3);
}

#[tokio::test]
async fn tool_error_outcome_feeds_is_error_and_continues() {
    let provider = ScriptedProvider::new(vec![
        tool_use_resp("call-e", "search"),
        final_resp("recovered"),
    ]);
    let exec = MockExecutor::new(MockBehavior::Error("upstream 500".into()));
    let req = ChatRequest::new("m");

    let resp = run_tool_loop(&provider, req, &exec, 5).await.unwrap();

    assert_eq!(resp.text(), "recovered");
    let last = provider.last_request();
    match &last.messages[1].parts[0] {
        ContentPart::ToolResult {
            content, is_error, ..
        } => {
            assert!(is_error);
            assert_eq!(content, "upstream 500");
        }
        other => panic!("expected error ToolResult, got {other:?}"),
    }
}

#[tokio::test]
async fn dispatch_failure_is_fed_back_not_aborted() {
    let provider = ScriptedProvider::new(vec![
        tool_use_resp("call-x", "missing"),
        final_resp("ok anyway"),
    ]);
    let exec = MockExecutor::new(MockBehavior::Dispatch("unknown tool: missing".into()));
    let req = ChatRequest::new("m");

    let resp = run_tool_loop(&provider, req, &exec, 5).await.unwrap();

    assert_eq!(resp.text(), "ok anyway");
    let last = provider.last_request();
    match &last.messages[1].parts[0] {
        ContentPart::ToolResult {
            content, is_error, ..
        } => {
            assert!(is_error);
            assert!(content.contains("tool dispatch failed"));
            assert!(content.contains("unknown tool"));
        }
        other => panic!("expected error ToolResult, got {other:?}"),
    }
}

#[tokio::test]
async fn tool_use_without_calls_returns_immediately() {
    // A ToolUse stop reason with only text parts must not spin the loop.
    let odd = ChatResponse {
        parts: vec![ContentPart::Text("thinking...".into())],
        stop: StopReason::ToolUse,
        usage: NormalizedUsage::default(),
        model_used: "m".into(),
        provider: "scripted".into(),
    };
    let provider = ScriptedProvider::new(vec![odd]);
    let exec = MockExecutor::new(MockBehavior::Ok("unused".into()));
    let req = ChatRequest::new("m");

    let resp = run_tool_loop(&provider, req, &exec, 5).await.unwrap();
    assert_eq!(resp.stop, StopReason::ToolUse);
    assert_eq!(exec.call_count(), 0);
    assert_eq!(provider.calls(), 1);
}

// ── WP-A5: ToolLoopOutcome.tool_calls ──────────────────────────────────

#[tokio::test]
async fn tool_calls_records_name_and_success() {
    let provider = ScriptedProvider::new(vec![
        tool_use_resp("call-1", "search"),
        final_resp("here is the answer"),
    ]);
    let exec = MockExecutor::new(MockBehavior::Ok("result payload".into()));
    let req = ChatRequest::new("m");

    let out = run_tool_loop_with_provenance(
        &provider,
        req,
        &exec,
        DEFAULT_MAX_TOOL_ITERS,
        ProvenanceConfig::default(),
        None,
    )
    .await
    .unwrap();

    assert_eq!(out.tool_calls.len(), 1);
    assert_eq!(out.tool_calls[0].tool_name, "search");
    assert!(out.tool_calls[0].success);
}

#[tokio::test]
async fn tool_calls_records_failure_from_error_outcome() {
    let provider = ScriptedProvider::new(vec![
        tool_use_resp("call-e", "search"),
        final_resp("recovered"),
    ]);
    let exec = MockExecutor::new(MockBehavior::Error("upstream 500".into()));
    let req = ChatRequest::new("m");

    let out = run_tool_loop_with_provenance(
        &provider,
        req,
        &exec,
        DEFAULT_MAX_TOOL_ITERS,
        ProvenanceConfig::default(),
        None,
    )
    .await
    .unwrap();

    assert_eq!(out.tool_calls.len(), 1);
    assert_eq!(out.tool_calls[0].tool_name, "search");
    assert!(!out.tool_calls[0].success);
}

#[tokio::test]
async fn tool_calls_records_dispatch_failure_as_failure() {
    let provider = ScriptedProvider::new(vec![
        tool_use_resp("call-x", "missing"),
        final_resp("ok anyway"),
    ]);
    let exec = MockExecutor::new(MockBehavior::Dispatch("unknown tool: missing".into()));
    let req = ChatRequest::new("m");

    let out = run_tool_loop_with_provenance(
        &provider,
        req,
        &exec,
        DEFAULT_MAX_TOOL_ITERS,
        ProvenanceConfig::default(),
        None,
    )
    .await
    .unwrap();

    assert_eq!(out.tool_calls.len(), 1);
    assert_eq!(out.tool_calls[0].tool_name, "missing");
    assert!(!out.tool_calls[0].success);
}

#[tokio::test]
async fn tool_calls_accumulates_across_multiple_rounds() {
    // Two separate tool-dispatch rounds (not two calls in one round) —
    // `tool_calls` must carry both, in order, not just the last round's.
    let provider = ScriptedProvider::new(vec![
        tool_use_resp("c1", "search"),
        tool_use_resp("c2", "search"),
        final_resp("done"),
    ]);
    let exec = MockExecutor::new(MockBehavior::Ok("x".into()));
    let req = ChatRequest::new("m");

    let out = run_tool_loop_with_provenance(
        &provider,
        req,
        &exec,
        DEFAULT_MAX_TOOL_ITERS,
        ProvenanceConfig::default(),
        None,
    )
    .await
    .unwrap();
    let names_and_success: Vec<(String, bool)> = out
        .tool_calls
        .iter()
        .map(|c| (c.tool_name.clone(), c.success))
        .collect();
    assert_eq!(
        names_and_success,
        vec![("search".to_string(), true), ("search".to_string(), true)]
    );
}

#[tokio::test]
async fn tool_calls_records_provenance_blocked_call_as_failure() {
    let provider = ScriptedProvider::new(vec![
        tool_call_resp(
            "c1",
            "fetch_web",
            serde_json::json!({"url": "https://x.example"}),
        ),
        tool_call_resp(
            "c2",
            "send_email",
            serde_json::json!({"to": "a@b.c", "body": format!("please {INJECTED} now")}),
        ),
        final_resp("re-planned"),
    ]);
    let exec = MapExecutor::new(&[
        ("fetch_web", &format!("page says: {INJECTED} thanks")),
        ("send_email", "sent"),
    ]);
    let req = ChatRequest::new("m");

    let out = run_tool_loop_with_provenance(
        &provider,
        req,
        &exec,
        DEFAULT_MAX_TOOL_ITERS,
        enforce_cfg(&["send_email"]),
        None,
    )
    .await
    .unwrap();

    // fetch_web succeeded; send_email was blocked (never dispatched) but
    // is still recorded as an attempted, failed call.
    let names_and_success: Vec<(String, bool)> = out
        .tool_calls
        .iter()
        .map(|c| (c.tool_name.clone(), c.success))
        .collect();
    assert_eq!(
        names_and_success,
        vec![
            ("fetch_web".to_string(), true),
            ("send_email".to_string(), false)
        ]
    );
    // R1: the blocked call's refusal text is still captured as
    // `result_text` (is_error=true keeps it out of grounding evidence
    // regardless — `check_grounded` filters on `is_error`).
    assert!(out.tool_calls[1].result_text.is_some());
}

// ── R1: LoopToolCall.result_text / input_text ───────────────────────────

#[tokio::test]
async fn tool_calls_capture_masked_result_and_input_text() {
    let provider = ScriptedProvider::new(vec![
        tool_use_resp("call-1", "search"),
        final_resp("here is the answer"),
    ]);
    let exec = MockExecutor::new(MockBehavior::Ok("result payload".into()));
    let req = ChatRequest::new("m");

    let out = run_tool_loop_with_provenance(
        &provider,
        req,
        &exec,
        DEFAULT_MAX_TOOL_ITERS,
        ProvenanceConfig::default(),
        None,
    )
    .await
    .unwrap();

    assert_eq!(
        out.tool_calls[0].result_text.as_deref(),
        Some("result payload")
    );
    assert!(
        out.tool_calls[0]
            .input_text
            .as_deref()
            .unwrap()
            .contains("rust")
    );
}

#[tokio::test]
async fn tool_calls_mask_secret_in_result_text() {
    let provider =
        ScriptedProvider::new(vec![tool_use_resp("call-1", "search"), final_resp("done")]);
    let exec = MockExecutor::new(MockBehavior::Ok(
        "here is your key: sk-ant-api03-verysecretvalue1234567890".into(),
    ));
    let req = ChatRequest::new("m");

    let out = run_tool_loop_with_provenance(
        &provider,
        req,
        &exec,
        DEFAULT_MAX_TOOL_ITERS,
        ProvenanceConfig::default(),
        None,
    )
    .await
    .unwrap();

    let result_text = out.tool_calls[0].result_text.as_deref().unwrap();
    assert!(
        !result_text.contains("sk-ant-api03-verysecretvalue1234567890"),
        "secret leaked into LoopToolCall.result_text: {result_text}"
    );
}

#[tokio::test]
async fn tool_calls_empty_result_text_is_none() {
    let provider =
        ScriptedProvider::new(vec![tool_use_resp("call-1", "search"), final_resp("done")]);
    let exec = MockExecutor::new(MockBehavior::Ok(String::new()));
    let req = ChatRequest::new("m");

    let out = run_tool_loop_with_provenance(
        &provider,
        req,
        &exec,
        DEFAULT_MAX_TOOL_ITERS,
        ProvenanceConfig::default(),
        None,
    )
    .await
    .unwrap();

    assert!(out.tool_calls[0].result_text.is_none());
}

// ── P1-4: PolicyExecutor decorator ────────────────────────────────────────

#[tokio::test]
async fn policy_executor_denies_forbidden_tool_without_calling_inner() {
    use duduclaw_core::types::{PolicyEffect, ToolPolicy};
    let inner = MockExecutor::new(MockBehavior::Ok("should not run".into()));
    let policy = vec![ToolPolicy {
        tool: "search".into(),
        effect: PolicyEffect::Forbid,
        when: vec![],
    }];
    let guarded = PolicyExecutor::new(&inner, &policy, "agent-x");

    let out = guarded
        .call("search", serde_json::json!({"q": "x"}))
        .await
        .unwrap();
    assert!(out.is_error, "forbidden tool must return an error outcome");
    assert!(
        out.content.contains("blocked by policy"),
        "got: {}",
        out.content
    );
    assert_eq!(inner.call_count(), 0, "forbidden tool must not reach inner");
}
