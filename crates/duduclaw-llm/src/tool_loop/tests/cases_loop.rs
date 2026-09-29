//! Tool-loop tests (cases_loop), moved verbatim out of `tool_loop.rs`.

use super::*;

#[tokio::test]
async fn ccr_bypass_preserves_generic_tool_result_without_storing_original() {
    let dir = tempfile::tempdir().unwrap();
    let store = crate::ccr::CcrStore::new(dir.path().join("ccr.db"));
    let content = "unverified MCP result\n".repeat(400);
    let executor =
        MockExecutor::new(MockBehavior::NoCcr(content.clone())).with_server("trusted-mcp");
    let provider =
        ScriptedProvider::new(vec![tool_use_resp("call-1", "search"), final_resp("done")]);
    let runtime = CcrRuntime::new_unrestricted_for_test(store.clone(), ccr_test_scope())
        .restrict_sources([("trusted-mcp".into(), "search".into())]);
    let outcome = run_tool_loop_with_provenance_and_ccr(
        &provider,
        ChatRequest::new("m"),
        &executor,
        3,
        ProvenanceConfig::default(),
        None,
        Some(runtime.clone()),
    )
    .await
    .unwrap();
    assert!(outcome.ccr_saved_results.is_empty());
    let sent = provider.last_request();
    let ContentPart::ToolResult {
        content: delivered,
        is_error,
        ..
    } = &sent.messages.last().unwrap().parts[0]
    else {
        panic!("missing tool result")
    };
    assert!(!is_error);
    assert_eq!(delivered, &content);
    assert!(runtime.find("unverified MCP result", 5).unwrap().is_empty());
    let source_key = runtime
        .source_key_for_call(Some("trusted-mcp"), "search")
        .unwrap();
    assert!(matches!(
        store.put(&ccr_test_scope(), &source_key, "call-1", &content),
        Err(CcrError::Revoked)
    ));
}

#[tokio::test]
async fn ccr_dense_json_keeps_exact_result_without_storing_preview() {
    let dir = tempfile::tempdir().unwrap();
    let store = crate::ccr::CcrStore::new(dir.path().join("ccr.db"));
    let rows: Vec<_> = (0..300)
        .map(|id| serde_json::json!({ "id": id, "exact_value": format!("value-{id:04}") }))
        .collect();
    let dense = serde_json::to_string(&rows).unwrap();
    let provider =
        ScriptedProvider::new(vec![tool_use_resp("call-1", "search"), final_resp("done")]);
    let exec = MockExecutor::new(MockBehavior::Ok(dense.clone()));
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
    let ContentPart::ToolResult { content, .. } = &sent.messages.last().unwrap().parts[0]
    else {
        panic!("missing tool result")
    };
    assert_eq!(content, &dense);
    let conn = rusqlite::Connection::open(store.path()).unwrap();
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM ccr_entries", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count, 0);
}

#[tokio::test]
async fn ccr_source_allowlist_blocks_unknown_server_and_rechecks_saved_route() {
    let dir = tempfile::tempdir().unwrap();
    let store = crate::ccr::CcrStore::new(dir.path().join("ccr.db"));
    let runtime = CcrRuntime::new_unrestricted_for_test(store.clone(), ccr_test_scope())
        .restrict_sources([("trusted-mcp".into(), "search".into())]);
    let raw = format!(
        "{}UNIQUE-MIDDLE{}",
        "ordinary result\n".repeat(300),
        "ordinary result\n".repeat(300)
    );

    let denied_provider =
        ScriptedProvider::new(vec![tool_use_resp("call-1", "search"), final_resp("done")]);
    let denied_exec =
        MockExecutor::new(MockBehavior::Ok(raw.clone())).with_server("untrusted-mcp");
    let denied_outcome = run_tool_loop_with_provenance_and_ccr(
        &denied_provider,
        ChatRequest::new("m"),
        &denied_exec,
        3,
        ProvenanceConfig::default(),
        None,
        Some(runtime.clone()),
    )
    .await
    .unwrap();
    assert!(denied_outcome.ccr_saved_results.is_empty());
    let sent = denied_provider.last_request();
    let ContentPart::ToolResult { content, .. } = &sent.messages.last().unwrap().parts[0]
    else {
        panic!("missing tool result")
    };
    assert_eq!(content, &raw);
    let conn = rusqlite::Connection::open(store.path()).unwrap();
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM ccr_entries", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count, 0);

    let allowed_provider =
        ScriptedProvider::new(vec![tool_use_resp("call-2", "search"), final_resp("done")]);
    let allowed_exec = MockExecutor::new(MockBehavior::Ok(raw)).with_server("trusted-mcp");
    run_tool_loop_with_provenance_and_ccr(
        &allowed_provider,
        ChatRequest::new("m"),
        &allowed_exec,
        3,
        ProvenanceConfig::default(),
        None,
        Some(runtime.clone()),
    )
    .await
    .unwrap();
    let sent = allowed_provider.last_request();
    let ContentPart::ToolResult { content, .. } = &sent.messages.last().unwrap().parts[0]
    else {
        panic!("missing tool result")
    };
    let id = content
        .split("id=")
        .nth(1)
        .unwrap()
        .split(']')
        .next()
        .unwrap();
    assert!(content.contains("[CCR:"));
    assert!(
        runtime
            .retrieve(id, Some("UNIQUE-MIDDLE"), 0, 64)
            .unwrap()
            .text
            .contains("UNIQUE-MIDDLE")
    );
    let removed = CcrRuntime::new_unrestricted_for_test(store, ccr_test_scope())
        .restrict_sources([("different-mcp".into(), "search".into())]);
    assert!(removed.retrieve(id, None, 0, 64).is_err());
}

#[tokio::test]
async fn ccr_internal_retrieve_is_scoped_and_does_not_dispatch_external_tool() {
    let dir = tempfile::tempdir().unwrap();
    let store = crate::ccr::CcrStore::new(dir.path().join("ccr.db"));
    let entry = store
        .put(&ccr_test_scope(), "search", "old-call", "甲乙丙 fatal 丁戊")
        .unwrap();
    let provider = ScriptedProvider::new(vec![
        ChatResponse {
            parts: vec![ContentPart::ToolCall {
                id: "retrieve-1".into(),
                name: CCR_RETRIEVE_TOOL.into(),
                args: serde_json::json!({"id": entry.id, "query": "fatal", "limit": 8}),
            }],
            stop: StopReason::ToolUse,
            usage: NormalizedUsage::default(),
            model_used: "m".into(),
            provider: "scripted".into(),
        },
        final_resp("done"),
    ]);
    let exec = MockExecutor::new(MockBehavior::Ok("should not dispatch".into()));
    let outcome = run_tool_loop_with_provenance_and_ccr(
        &provider,
        ChatRequest::new("m"),
        &exec,
        3,
        ProvenanceConfig::default(),
        None,
        Some(CcrRuntime::new_unrestricted_for_test(
            store,
            ccr_test_scope(),
        )),
    )
    .await
    .unwrap();
    assert_eq!(outcome.telemetry.ccr_retrieve_attempts, 1);
    assert_eq!(outcome.telemetry.ccr_retrieve_successes, 1);
    assert_eq!(outcome.telemetry.ccr_retrieve_misses, 0);
    assert_eq!(outcome.telemetry.ccr_retrieved_bytes, "fatal ".len() as u64);
    assert_eq!(exec.call_count(), 0);
    let sent = provider.last_request();
    let ContentPart::ToolResult {
        content, is_error, ..
    } = &sent.messages.last().unwrap().parts[0]
    else {
        panic!("missing tool result")
    };
    assert!(!is_error);
    assert!(content.contains("fatal"));
}

#[tokio::test]
async fn ccr_internal_find_discovers_prior_turn_without_external_dispatch() {
    let dir = tempfile::tempdir().unwrap();
    let store = crate::ccr::CcrStore::new(dir.path().join("ccr.db"));
    let entry = store
        .put(
            &ccr_test_scope(),
            "search",
            "earlier-turn",
            "甲乙 UNIQUE-MIDDLE 丁戊",
        )
        .unwrap();
    let provider = ScriptedProvider::new(vec![
        ChatResponse {
            parts: vec![ContentPart::ToolCall {
                id: "find-1".into(),
                name: CCR_FIND_TOOL.into(),
                args: serde_json::json!({"query": "UNIQUE-MIDDLE", "limit": 1}),
            }],
            stop: StopReason::ToolUse,
            usage: NormalizedUsage::default(),
            model_used: "m".into(),
            provider: "scripted".into(),
        },
        final_resp("done"),
    ]);
    let exec = MockExecutor::new(MockBehavior::Ok("should not dispatch".into()));
    let outcome = run_tool_loop_with_provenance_and_ccr(
        &provider,
        ChatRequest::new("m"),
        &exec,
        3,
        ProvenanceConfig::default(),
        None,
        Some(CcrRuntime::new_unrestricted_for_test(
            store,
            ccr_test_scope(),
        )),
    )
    .await
    .unwrap();
    assert_eq!(outcome.telemetry.ccr_find_attempts, 1);
    assert_eq!(outcome.telemetry.ccr_find_hits, 1);
    assert_eq!(outcome.telemetry.ccr_find_misses, 0);
    assert_eq!(exec.call_count(), 0);
    let sent = provider.last_request();
    let find_tool = sent
        .tools
        .iter()
        .find(|tool| tool.name == CCR_FIND_TOOL)
        .unwrap();
    assert!(find_tool.description.contains("UTF-8 bytes"));
    assert!(
        find_tool.input_schema["properties"]["query"]
            .get("minLength")
            .is_none()
    );
    assert!(
        find_tool.input_schema["properties"]["query"]
            .get("maxLength")
            .is_none()
    );
    let ContentPart::ToolResult {
        content, is_error, ..
    } = &sent.messages.last().unwrap().parts[0]
    else {
        panic!("missing find result")
    };
    assert!(!is_error);
    let value: Value = serde_json::from_str(content).unwrap();
    assert_eq!(value["hits"][0]["id"], entry.id);
    assert_eq!(value["hits"][0]["exact_phrase"], true);
    assert_eq!(value["hits"].as_array().unwrap().len(), 1);
}

/// Regression (W3-1 #3): `duduclaw_ccr_find` is model-visible and each
/// call scans the whole scope, so one loop gets a fixed budget. The 9th
/// call must come back as an explicit `is_error` tool result (never a
/// silent empty answer) and be counted.
#[tokio::test]
async fn ccr_find_is_capped_per_tool_loop_and_the_ninth_call_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let store = crate::ccr::CcrStore::new(dir.path().join("ccr.db"));
    store
        .put(
            &ccr_test_scope(),
            "search",
            "earlier-turn",
            "甲乙 UNIQUE-MIDDLE 丁戊",
        )
        .unwrap();
    let find_call = || ChatResponse {
        parts: vec![ContentPart::ToolCall {
            id: "find-n".into(),
            name: CCR_FIND_TOOL.into(),
            args: serde_json::json!({"query": "UNIQUE-MIDDLE", "limit": 1}),
        }],
        stop: StopReason::ToolUse,
        usage: NormalizedUsage::default(),
        model_used: "m".into(),
        provider: "scripted".into(),
    };
    let rounds = CCR_FIND_MAX_CALLS_PER_LOOP as usize + 1;
    let mut script: Vec<ChatResponse> = (0..rounds).map(|_| find_call()).collect();
    script.push(final_resp("done"));
    let provider = ScriptedProvider::new(script);
    let outcome = run_tool_loop_with_provenance_and_ccr(
        &provider,
        ChatRequest::new("m"),
        &MockExecutor::new(MockBehavior::Ok("should not dispatch".into())),
        rounds + 1,
        ProvenanceConfig::default(),
        None,
        Some(CcrRuntime::new_unrestricted_for_test(
            store,
            ccr_test_scope(),
        )),
    )
    .await
    .unwrap();
    assert_eq!(outcome.telemetry.ccr_find_rate_limited, 1);
    assert_eq!(
        outcome.telemetry.ccr_find_hits,
        CCR_FIND_MAX_CALLS_PER_LOOP,
        "only the budgeted calls may reach the store"
    );
    let refused = provider
        .seen
        .lock()
        .unwrap()
        .iter()
        .flat_map(|request| request.messages.iter())
        .flat_map(|message| message.parts.iter())
        .filter_map(|part| match part {
            ContentPart::ToolResult {
                content, is_error, ..
            } if *is_error => Some(content.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(refused.len(), 1, "{refused:?}");
    assert!(refused[0].contains("CCR search budget exhausted"), "{refused:?}");
}

/// Regression (W3-1 #5): the tool loop's own logs must carry the handle
/// digest the audit table stores, never the plaintext handle.
#[test]
fn ccr_handle_log_digest_is_eight_hex_chars_of_the_sha256() {
    let digest = crate::ccr::handle_log_digest("handle-abc");
    assert_eq!(digest.len(), 8);
    assert!(digest.chars().all(|c| c.is_ascii_hexdigit()));
    assert_ne!(digest, "handle-a");
    assert_eq!(digest, crate::ccr::handle_log_digest("handle-abc"));
    assert_ne!(digest, crate::ccr::handle_log_digest("handle-abd"));
}

/// Regression (W3-1 #2): `still_valid()` is the async, off-reactor form;
/// an empty set answers without the blocking pool and a lost lease is
/// still observed through it.
#[tokio::test]
async fn ccr_delivery_guards_revalidate_off_the_reactor() {
    let empty = CcrDeliveryGuards::default();
    assert!(empty.still_valid().await);
    let live = Arc::new(AtomicBool::new(true));
    let mut guards = CcrDeliveryGuards::default();
    guards.push(Arc::new(FixtureDeliveryLease {
        live: live.clone(),
        drops: Arc::new(AtomicUsize::new(0)),
    }));
    assert!(guards.still_valid().await);
    assert!(guards.still_valid_blocking());
    live.store(false, Ordering::SeqCst);
    assert!(!guards.still_valid().await);
    assert!(!guards.still_valid_blocking());
}

#[tokio::test]
async fn ccr_missing_retrieval_id_is_audited_without_dispatch() {
    let dir = tempfile::tempdir().unwrap();
    let store = crate::ccr::CcrStore::new(dir.path().join("ccr.db"));
    let provider = ScriptedProvider::new(vec![
        ChatResponse {
            parts: vec![ContentPart::ToolCall {
                id: "retrieve-1".into(),
                name: CCR_RETRIEVE_TOOL.into(),
                args: serde_json::json!({}),
            }],
            stop: StopReason::ToolUse,
            usage: NormalizedUsage::default(),
            model_used: "m".into(),
            provider: "scripted".into(),
        },
        final_resp("done"),
    ]);
    let exec = MockExecutor::new(MockBehavior::Ok("should not dispatch".into()));
    let out = run_tool_loop_with_provenance_and_ccr(
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
    assert_eq!(exec.call_count(), 0);
    assert_eq!(out.telemetry.provider_rounds, 2);
    assert_eq!(out.telemetry.usage_reported_rounds, 0);
    assert_eq!(out.telemetry.ccr_retrieve_attempts, 1);
    assert_eq!(out.telemetry.ccr_retrieve_successes, 0);
    assert_eq!(out.telemetry.ccr_retrieve_misses, 1);
    let conn = rusqlite::Connection::open(store.path()).unwrap();
    let status: String = conn
        .query_row(
            "SELECT status FROM ccr_retrieval_audit ORDER BY audit_id DESC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(status, "refused");
}

#[tokio::test]
async fn ccr_search_preview_retains_requested_match() {
    let dir = tempfile::tempdir().unwrap();
    let store = crate::ccr::CcrStore::new(dir.path().join("ccr.db"));
    let original = format!(
        "{}\ncase 791: exact needle is present\n{}",
        "ordinary result\n".repeat(400),
        "ordinary result\n".repeat(400)
    );
    let provider = ScriptedProvider::new(vec![
        ChatResponse {
            parts: vec![ContentPart::ToolCall {
                id: "search-1".into(),
                name: "search".into(),
                args: serde_json::json!({"query": "exact needle"}),
            }],
            stop: StopReason::ToolUse,
            usage: NormalizedUsage::default(),
            model_used: "m".into(),
            provider: "scripted".into(),
        },
        final_resp("done"),
    ]);
    let exec = MockExecutor::new(MockBehavior::Ok(original));
    run_tool_loop_with_provenance_and_ccr(
        &provider,
        ChatRequest::new("m"),
        &exec,
        3,
        ProvenanceConfig::default(),
        None,
        Some(CcrRuntime::new_unrestricted_for_test(
            store,
            ccr_test_scope(),
        )),
    )
    .await
    .unwrap();
    let sent = provider.last_request();
    let ContentPart::ToolResult { content, .. } = &sent.messages.last().unwrap().parts[0]
    else {
        panic!("missing tool result")
    };
    assert!(content.contains("case 791: exact needle is present"));
    assert!(content.contains("[CCR:"));
}

#[tokio::test]
async fn ccr_revoked_call_never_falls_back_to_raw_result() {
    let dir = tempfile::tempdir().unwrap();
    let store = crate::ccr::CcrStore::new(dir.path().join("ccr.db"));
    store
        .revoke_source_call(&ccr_test_scope(), "search", "call-1")
        .unwrap();
    let provider =
        ScriptedProvider::new(vec![tool_use_resp("call-1", "search"), final_resp("done")]);
    let exec = MockExecutor::new(MockBehavior::Ok("sensitive-data ".repeat(600)));
    let outcome = run_tool_loop_with_provenance_and_ccr(
        &provider,
        ChatRequest::new("m"),
        &exec,
        3,
        ProvenanceConfig::default(),
        None,
        Some(CcrRuntime::new_unrestricted_for_test(
            store,
            ccr_test_scope(),
        )),
    )
    .await
    .unwrap();
    assert!(outcome.ccr_saved_results.is_empty());
    let sent = provider.last_request();
    let ContentPart::ToolResult {
        content, is_error, ..
    } = &sent.messages.last().unwrap().parts[0]
    else {
        panic!("missing tool result")
    };
    assert!(*is_error);
    assert!(content.contains("revoked"));
    assert!(!content.contains("sensitive-data"));
}
