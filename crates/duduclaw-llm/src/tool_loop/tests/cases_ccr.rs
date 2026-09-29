//! Tool-loop tests (cases_ccr), moved verbatim out of `tool_loop.rs`.

use super::*;
use super::super::telemetry::{CCR_TELEMETRY_PENDING_LIMIT, try_ccr_telemetry_permit};

#[tokio::test]
async fn ccr_delivery_lease_survives_model_request_and_is_not_copied_into_tool_event() {
    let (_dir, runtime, id, _live, drops) = leased_ccr_fixture();
    let provider = ScriptedProvider::new(vec![ccr_retrieve_call(&id), final_resp("done")]);
    let outcome = run_tool_loop_with_provenance_and_ccr(
        &provider,
        ChatRequest::new("m"),
        &MockExecutor::new(MockBehavior::Ok("unused".into())),
        2,
        ProvenanceConfig::default(),
        None,
        Some(runtime),
    )
    .await
    .unwrap();
    assert_eq!(provider.calls(), 2);
    assert!(!outcome.ccr_delivery_guards.is_empty());
    assert!(outcome.ccr_delivery_guards.still_valid().await);
    assert_eq!(drops.load(Ordering::SeqCst), 0);
    let sent = provider.last_request();
    let ContentPart::ToolResult { content, .. } = &sent.messages.last().unwrap().parts[0]
    else {
        panic!("missing CCR tool result")
    };
    assert!(content.contains("lease-bound original passage"));
    assert!(
        !outcome.tool_calls[0]
            .result_text
            .as_deref()
            .unwrap()
            .contains("lease-bound original passage")
    );
    drop(outcome);
    assert_eq!(drops.load(Ordering::SeqCst), 1);
}

#[test]
fn ccr_delivery_guard_refuses_direct_ccr_call_revocation() {
    let (_dir, runtime, id, _live, drops) = leased_ccr_fixture();
    let chunk = runtime.retrieve(&id, None, 0, 128).unwrap();
    let guard = chunk.delivery_guard().unwrap();
    assert!(guard.still_valid());
    runtime
        .store
        .revoke_source_call(&runtime.scope, "search", "seed")
        .unwrap();
    assert!(!guard.still_valid());
    drop(chunk);
    drop(guard);
    assert_eq!(drops.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn ccr_delivery_lease_expiring_during_model_request_discards_response() {
    let (_dir, runtime, id, live, drops) = leased_ccr_fixture();
    let provider = ExpiringLeaseProvider {
        call: ccr_retrieve_call(&id),
        live,
        calls: AtomicUsize::new(0),
    };
    let result = run_tool_loop_with_provenance_and_ccr(
        &provider,
        ChatRequest::new("m"),
        &MockExecutor::new(MockBehavior::Ok("unused".into())),
        2,
        ProvenanceConfig::default(),
        None,
        Some(runtime),
    )
    .await;
    assert!(
        matches!(result, Err(LlmError::InvalidRequest(message)) if message.contains("CCR source expired"))
    );
    assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
    assert_eq!(drops.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn first_verified_source_delivery_is_guarded_even_without_a_ccr_preview() {
    let (_dir, runtime, _id, live, drops) = leased_ccr_fixture();
    let provider = ExpiringLeaseProvider {
        call: tool_use_resp("fresh-source", "search"),
        live,
        calls: AtomicUsize::new(0),
    };
    let executor = MockExecutor::new(MockBehavior::Bound(
        "lease-bound original passage ".repeat(100),
        CcrSourceArtifact {
            connector: "fixture".into(),
            artifact_id: "source-1".into(),
            version: "v1".into(),
            acl_revision: "acl-1".into(),
        },
    ));
    let result = run_tool_loop_with_provenance_and_ccr(
        &provider,
        ChatRequest::new("m"),
        &executor,
        2,
        ProvenanceConfig::default(),
        None,
        Some(runtime),
    )
    .await;
    assert!(
        matches!(result, Err(LlmError::InvalidRequest(message)) if message.contains("CCR source expired"))
    );
    assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
    assert_eq!(drops.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn transformed_verified_source_keeps_lease_through_model_request() {
    let (_dir, runtime, _id, live, drops) = leased_ccr_fixture();
    let provider = ExpiringLeaseProvider {
        call: tool_use_resp("fresh-source", "search"),
        live,
        calls: AtomicUsize::new(0),
    };
    let executor = MockExecutor::new(MockBehavior::Bound(
        "lease-bound original passage ".repeat(100),
        CcrSourceArtifact {
            connector: "fixture".into(),
            artifact_id: "source-1".into(),
            version: "v1".into(),
            acl_revision: "acl-1".into(),
        },
    ));
    let result = run_tool_loop_with_provenance_and_ccr(
        &provider,
        ChatRequest::new("m"),
        &executor,
        2,
        ProvenanceConfig::default(),
        Some(Arc::new(SpyInterceptor::allow_all())),
        Some(runtime),
    )
    .await;
    assert!(matches!(
        result,
        Err(LlmError::InvalidRequest(message)) if message.contains("CCR source expired")
    ));
    assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
    assert_eq!(drops.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn verified_source_without_ccr_runtime_is_withheld() {
    let raw = "private verified source".repeat(100);
    let artifact = CcrSourceArtifact {
        connector: "causal".into(),
        artifact_id: "source-1".into(),
        version: "v1".into(),
        acl_revision: "acl-1".into(),
    };
    let provider = ScriptedProvider::new(vec![
        tool_use_resp("fresh-source", "search"),
        final_resp("done"),
    ]);
    let executor = MockExecutor::new(MockBehavior::Bound(raw.clone(), artifact));
    let outcome = run_tool_loop_with_provenance_and_ccr(
        &provider,
        ChatRequest::new("m"),
        &executor,
        2,
        ProvenanceConfig::default(),
        None,
        None,
    )
    .await
    .unwrap();
    assert!(outcome.ccr_delivery_guards.is_empty());
    let sent = provider.last_request();
    let ContentPart::ToolResult {
        content, is_error, ..
    } = &sent.messages.last().unwrap().parts[0]
    else {
        panic!("missing tool result")
    };
    assert!(*is_error);
    assert!(content.contains("withheld"));
    assert!(!content.contains(&raw));
}

#[tokio::test]
async fn ccr_stores_post_interceptor_original_before_marker() {
    let dir = tempfile::tempdir().unwrap();
    let store = crate::ccr::CcrStore::new(dir.path().join("ccr.db"));
    let runtime = CcrRuntime::new_unrestricted_for_test(store.clone(), ccr_test_scope());
    let raw = format!(
        "{}UNIQUE-MIDDLE{}",
        "sensitive-result ".repeat(200),
        "sensitive-result ".repeat(200)
    );
    let mut tool_round = tool_use_resp("call-1", "search");
    tool_round.usage = NormalizedUsage {
        input_tokens: 100,
        output_tokens: 20,
        cache_read_tokens: 50,
        ..Default::default()
    };
    let mut final_round = final_resp("done");
    final_round.usage = NormalizedUsage {
        input_tokens: 110,
        output_tokens: 30,
        cache_read_tokens: 60,
        ..Default::default()
    };
    let provider = ScriptedProvider::new(vec![tool_round, final_round]);
    let exec = MockExecutor::new(MockBehavior::Ok(raw.clone()));
    let interceptor = std::sync::Arc::new(SpyInterceptor::allow_all());
    let outcome = run_tool_loop_with_provenance_and_ccr(
        &provider,
        ChatRequest::new("m"),
        &exec,
        3,
        ProvenanceConfig::default(),
        Some(interceptor),
        Some(runtime),
    )
    .await
    .unwrap();
    assert_eq!(outcome.telemetry.provider_rounds, 2);
    assert_eq!(outcome.telemetry.usage_reported_rounds, 2);
    assert_eq!(outcome.telemetry.provider_usage.input_tokens, 210);
    assert_eq!(outcome.telemetry.provider_usage.output_tokens, 50);
    assert_eq!(outcome.telemetry.provider_usage.cache_read_tokens, 110);
    assert_eq!(outcome.telemetry.ccr_compressed_results, 1);
    assert!(outcome.telemetry.ccr_original_bytes > outcome.telemetry.ccr_delivered_bytes);
    let conn = rusqlite::Connection::open(store.path()).unwrap();
    let mut persisted = false;
    for _ in 0..100 {
        if matches!(
            conn.query_row(
                "SELECT COUNT(*),SUM(provider_rounds),SUM(ccr_compressed_results)
             FROM ccr_loop_telemetry WHERE tenant_id='tenant-a'",
                [],
                |row| Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?
                )),
            ),
            Ok((1, 2, 1))
        ) {
            persisted = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(persisted, "completed CCR loop telemetry was not persisted");

    let sent = provider.last_request();
    assert!(sent.tools.iter().any(|t| t.name == CCR_RETRIEVE_TOOL));
    let ContentPart::ToolResult { content, .. } = &sent.messages.last().unwrap().parts[0]
    else {
        panic!("missing tool result")
    };
    assert!(content.contains("[CCR:"));
    assert!(
        !content.contains("UNIQUE-MIDDLE"),
        "full result must not leak into preview"
    );
    let id = content
        .split("id=")
        .nth(1)
        .unwrap()
        .split(']')
        .next()
        .unwrap();
    let restored = store.retrieve(&ccr_test_scope(), id, None, 0, 64).unwrap();
    assert_eq!(outcome.ccr_saved_results.len(), 1);
    let saved = &outcome.ccr_saved_results[0];
    assert_eq!(saved.scope, ccr_test_scope());
    assert_eq!(saved.source_tool, "search");
    assert_eq!(saved.source_call_id, "call-1");
    assert_eq!(saved.id, id);
    assert_eq!(saved.original_bytes, restored.total_bytes);
    assert!(saved.expires_at > 0);
    assert!(
        restored
            .text
            .starts_with(&format!("{SPY_TEXT_REDACTION_MARKER}sensitive-result"))
    );
    assert!(
        !restored.text.starts_with("sensitive-result"),
        "store must run after interceptor"
    );
}

#[tokio::test]
async fn telemetry_write_failure_does_not_change_completed_loop_response() {
    let dir = tempfile::tempdir().unwrap();
    let store = crate::ccr::CcrStore::new(dir.path().join("ccr.db"));
    store
        .put(&ccr_test_scope(), "search", "seed", "short")
        .unwrap();
    let conn = rusqlite::Connection::open(store.path()).unwrap();
    conn.execute_batch("CREATE TABLE ccr_loop_telemetry (broken INTEGER)")
        .unwrap();
    let outcome = run_tool_loop_with_provenance_and_ccr(
        &ScriptedProvider::new(vec![final_resp("still answered")]),
        ChatRequest::new("m"),
        &MockExecutor::new(MockBehavior::Ok("unused".into())),
        1,
        ProvenanceConfig::default(),
        None,
        Some(CcrRuntime::new_unrestricted_for_test(
            store,
            ccr_test_scope(),
        )),
    )
    .await
    .unwrap();
    assert_eq!(outcome.response.text(), "still answered");
    assert_eq!(outcome.telemetry.provider_rounds, 1);
}

#[test]
fn telemetry_schedule_rejects_work_when_slots_are_occupied() {
    let pool = Arc::new(Semaphore::new(CCR_TELEMETRY_PENDING_LIMIT));
    let permits = (0..CCR_TELEMETRY_PENDING_LIMIT)
        .map(|_| try_ccr_telemetry_permit(&pool).unwrap())
        .collect::<Vec<_>>();
    assert!(try_ccr_telemetry_permit(&pool).is_none());
    drop(permits);
    assert!(try_ccr_telemetry_permit(&pool).is_some());
}

#[tokio::test]
async fn locked_telemetry_database_does_not_delay_completed_loop() {
    let dir = tempfile::tempdir().unwrap();
    let store = crate::ccr::CcrStore::new(dir.path().join("ccr.db"));
    store
        .put(&ccr_test_scope(), "search", "seed", "short")
        .unwrap();
    let lock = rusqlite::Connection::open(store.path()).unwrap();
    lock.execute_batch("BEGIN IMMEDIATE").unwrap();
    let started = Instant::now();
    let outcome = run_tool_loop_with_provenance_and_ccr(
        &ScriptedProvider::new(vec![final_resp("still answered")]),
        ChatRequest::new("m"),
        &MockExecutor::new(MockBehavior::Ok("unused".into())),
        1,
        ProvenanceConfig::default(),
        None,
        Some(CcrRuntime::new_unrestricted_for_test(
            store,
            ccr_test_scope(),
        )),
    )
    .await
    .unwrap();
    assert_eq!(outcome.response.text(), "still answered");
    assert!(started.elapsed() < std::time::Duration::from_millis(200));
    lock.execute_batch("ROLLBACK").unwrap();
}

#[tokio::test]
async fn ccr_tool_loop_binds_verified_artifact_and_rejects_revoked_handle() {
    let dir = tempfile::tempdir().unwrap();
    let store = crate::ccr::CcrStore::new(dir.path().join("ccr.db"));
    let artifact = CcrSourceArtifact {
        connector: "trusted-support".into(),
        artifact_id: "ticket-42".into(),
        version: "v7".into(),
        acl_revision: "acl-3".into(),
    };
    let provider =
        ScriptedProvider::new(vec![tool_use_resp("call-1", "search"), final_resp("done")]);
    let content = "support evidence\n".repeat(400);
    let executor = MockExecutor::new(MockBehavior::Bound(content.clone(), artifact.clone()))
        .with_server("trusted-mcp");
    let runtime = CcrRuntime::new_unrestricted_for_test(store.clone(), ccr_test_scope())
        .restrict_sources([("trusted-mcp".into(), "search".into())])
        .with_bound_source_validator(Arc::new(FixtureBoundSource {
            scope: ccr_test_scope(),
            artifact: artifact.clone(),
            digest: format!("{:x}", sha2::Sha256::digest(content.as_bytes())),
        }));
    run_tool_loop_with_provenance_and_ccr(
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
    let sent = provider.last_request();
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
    assert!(runtime.retrieve(id, None, 0, 100).is_ok());
    assert_eq!(
        store
            .revoke_artifact_version(
                &ccr_test_scope().tenant_id,
                &artifact.connector,
                &artifact.artifact_id,
                &artifact.version,
            )
            .unwrap(),
        1
    );
    assert!(matches!(
        runtime.retrieve(id, None, 0, 100),
        Err(CcrError::NotFound)
    ));
    assert!(runtime.find("support evidence", 5).unwrap().is_empty());
}

#[tokio::test]
async fn ccr_attested_source_changed_after_write_scrubs_entry_before_marker() {
    let dir = tempfile::tempdir().unwrap();
    let store = crate::ccr::CcrStore::new(dir.path().join("ccr.db"));
    let executor = AttestationRaceExecutor {
        content: "source evidence\n".repeat(400),
        artifact: CcrSourceArtifact {
            connector: "causal".into(),
            artifact_id: "artifact".into(),
            version: "v1".into(),
            acl_revision: "acl".into(),
        },
        rechecks: std::sync::atomic::AtomicUsize::new(0),
        fail_on_recheck: 2,
    };
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
    assert_eq!(
        executor.rechecks.load(std::sync::atomic::Ordering::SeqCst),
        2
    );
    assert!(outcome.ccr_saved_results.is_empty());
    let sent = provider.last_request();
    let ContentPart::ToolResult {
        content, is_error, ..
    } = &sent.messages.last().unwrap().parts[0]
    else {
        panic!("missing tool result")
    };
    assert!(*is_error);
    assert!(content.contains("withheld"));
    assert!(!content.contains("source evidence"));
    assert!(runtime.find("source evidence", 5).unwrap().is_empty());
    assert!(matches!(
        store.put_bound(
            &ccr_test_scope(),
            "search",
            "new-call",
            "source evidence",
            &executor.artifact,
        ),
        Err(CcrError::Revoked)
    ));
}

#[tokio::test]
async fn ccr_unverified_artifact_claim_is_withheld() {
    let dir = tempfile::tempdir().unwrap();
    let store = crate::ccr::CcrStore::new(dir.path().join("ccr.db"));
    let executor = AttestationRaceExecutor {
        content: "source evidence\n".repeat(400),
        artifact: CcrSourceArtifact {
            connector: "causal".into(),
            artifact_id: "artifact".into(),
            version: "v1".into(),
            acl_revision: "acl".into(),
        },
        rechecks: std::sync::atomic::AtomicUsize::new(0),
        fail_on_recheck: 1,
    };
    let provider =
        ScriptedProvider::new(vec![tool_use_resp("call-1", "search"), final_resp("done")]);
    let runtime = CcrRuntime::new_unrestricted_for_test(store, ccr_test_scope())
        .restrict_sources([("trusted-mcp".into(), "search".into())]);
    let outcome = run_tool_loop_with_provenance_and_ccr(
        &provider,
        ChatRequest::new("m"),
        &executor,
        3,
        ProvenanceConfig::default(),
        None,
        Some(runtime),
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
    assert!(content.contains("withheld"));
    assert!(!content.contains("source evidence"));
}

#[tokio::test]
async fn interceptor_rewrite_does_not_release_a_source_revoked_after_attestation() {
    let dir = tempfile::tempdir().unwrap();
    let store = crate::ccr::CcrStore::new(dir.path().join("ccr.db"));
    let executor = AttestationRaceExecutor {
        content: "private source evidence\n".repeat(400),
        artifact: CcrSourceArtifact {
            connector: "causal".into(),
            artifact_id: "artifact".into(),
            version: "v1".into(),
            acl_revision: "acl".into(),
        },
        rechecks: std::sync::atomic::AtomicUsize::new(0),
        // The first recheck rejects rewritten bytes. The second checks
        // original bytes after the source has been revoked.
        fail_on_recheck: 2,
    };
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
        Some(std::sync::Arc::new(SpyInterceptor::allow_all())),
        Some(runtime.clone()),
    )
    .await
    .unwrap();
    assert_eq!(
        executor.rechecks.load(std::sync::atomic::Ordering::SeqCst),
        2
    );
    assert!(outcome.ccr_saved_results.is_empty());
    let sent = provider.last_request();
    let ContentPart::ToolResult {
        content, is_error, ..
    } = &sent.messages.last().unwrap().parts[0]
    else {
        panic!("missing tool result")
    };
    assert!(*is_error);
    assert!(content.contains("withheld"));
    assert!(!content.contains("private source evidence"));
    assert!(
        runtime
            .find("private source evidence", 5)
            .unwrap()
            .is_empty()
    );
}
