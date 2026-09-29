//! The loop body itself — [`run_tool_loop_with_provenance_and_ccr`].
//! Moved verbatim out of `tool_loop.rs`.

use super::*;
use super::outcome::mask_and_cap;
use super::telemetry::{CCR_FIND_RATE_LIMITED_MESSAGE, finish_tool_loop};

/// Tool loop with scoped, durable compression of post-redaction tool results.
/// Existing callers use [`run_tool_loop_with_provenance`] and remain unchanged.
pub async fn run_tool_loop_with_provenance_and_ccr(
    provider: &dyn ChatProvider,
    mut req: ChatRequest,
    tools: &dyn ToolExecutor,
    max_iters: usize,
    mut cfg: ProvenanceConfig,
    interceptor: Option<std::sync::Arc<dyn ToolInterceptor>>,
    ccr: Option<CcrRuntime>,
) -> Result<ToolLoopOutcome, LlmError> {
    let started = Instant::now();
    let mut telemetry = ToolLoopTelemetry::default();
    // Seed the tool schemas unless the caller supplied their own.
    if req.tools.is_empty() {
        req.tools = tools.defs();
    }
    // O7: the CCR tool schemas come from the single registration authority in
    // `duduclaw_core::tool_catalog`, not from literals inlined here. A caller
    // that already supplied its own definition for either name keeps it.
    if ccr.is_some() {
        for schema in duduclaw_core::tool_catalog::ccr_tool_schemas() {
            if req.tools.iter().any(|tool| tool.name == schema.name) {
                continue;
            }
            req.tools.push(crate::types::ToolDef {
                name: schema.name.into(),
                description: schema.description.into(),
                input_schema: schema.input_schema,
            });
        }
    }
    // A zero cap is meaningless; clamp to at least one round.
    let cap = max_iters.max(1);

    // Off ⇒ no ledger at all; every provenance branch below is skipped.
    let mut ledger = if cfg.policy == ProvenancePolicy::Off {
        None
    } else {
        Some(
            cfg.initial_ledger
                .take()
                .unwrap_or_else(|| seed_default_ledger(&req)),
        )
    };
    let mut flags: Vec<ProvenanceFlag> = Vec::new();
    // WP-A5/R1: every dispatched (or refused) tool call across the whole loop.
    let mut tool_calls: Vec<LoopToolCall> = Vec::new();
    let mut ccr_saved_results = Vec::new();
    let mut ccr_delivery_guards = CcrDeliveryGuards::default();
    // Per-scope `duduclaw_ccr_find` budget (the loop is bound to one scope).
    let mut ccr_find_calls: u64 = 0;

    let mut last = provider.complete(&req).await?;
    telemetry.record_provider(&last);

    for _ in 0..cap {
        if last.stop != StopReason::ToolUse {
            if !ccr_delivery_guards.still_valid().await {
                return Err(LlmError::InvalidRequest(
                    "CCR source expired or revoked before delivery".into(),
                ));
            }
            return Ok(finish_tool_loop(
                last,
                flags,
                tool_calls,
                ccr_saved_results,
                ccr_delivery_guards,
                telemetry,
                started,
                ccr.as_ref(),
            ));
        }
        let calls = tool_calls_of(&last);
        if calls.is_empty() {
            // Model signalled ToolUse but emitted no ToolCall parts — nothing
            // to dispatch; surface as-is rather than spin.
            if !ccr_delivery_guards.still_valid().await {
                return Err(LlmError::InvalidRequest(
                    "CCR source expired or revoked before delivery".into(),
                ));
            }
            return Ok(finish_tool_loop(
                last,
                flags,
                tool_calls,
                ccr_saved_results,
                ccr_delivery_guards,
                telemetry,
                started,
                ccr.as_ref(),
            ));
        }

        // Echo the assistant turn verbatim (keeps Reasoning signatures for
        // providers that require thinking replay), then answer with results.
        req.messages.push(ChatMessage {
            role: Role::Assistant,
            parts: last.parts.clone(),
        });

        let mut result_parts = Vec::with_capacity(calls.len());
        for (id, name, args) in calls {
            let is_ccr_find = name == CCR_FIND_TOOL;
            let is_ccr_retrieve = name == CCR_RETRIEVE_TOOL;
            if is_ccr_find {
                telemetry.ccr_find_attempts = telemetry.ccr_find_attempts.saturating_add(1);
            }
            if is_ccr_retrieve {
                telemetry.ccr_retrieve_attempts = telemetry.ccr_retrieve_attempts.saturating_add(1);
            }
            let mut ccr_find_had_hit = false;
            let mut ccr_returned_bytes = 0_u64;
            let mut ccr_preview_sizes: Option<(usize, usize)> = None;
            let mut saved_result: Option<CcrSavedResult> = None;
            let mut ccr_revoke_call = false;
            // R1: capture the call's own serialized arguments BEFORE `args`
            // is (possibly) moved into `tools.call(&name, args)` below —
            // `Value::to_string()` only borrows, so this is safe regardless
            // of which branch actually dispatches.
            let input_text = mask_and_cap(&args.to_string(), LOOP_TOOL_CALL_INPUT_MAX_CHARS);

            // Provenance gate (S2): decide before dispatch.
            let block_reason = match &ledger {
                Some(ledger) => {
                    let decision = evaluate_call(&cfg, ledger, &name, &args);
                    flags.extend(decision.flags);
                    decision.block_reason
                }
                None => None,
            };

            // RFC-23 §13.6 egress gate. Runs after the provenance gate (a
            // provenance-blocked call is already refused; there is nothing to
            // restore) and before dispatch, so a `Deny` never reaches the tool.
            let server = interceptor
                .as_ref()
                .and_then(|_| tools.server_of(&name))
                .unwrap_or_default();
            let (args, intercept_denial) = match (&interceptor, &block_reason) {
                (Some(icept), None) => match icept.before_call(&server, &name, args) {
                    InterceptDecision::Allow(a) => (a, None),
                    InterceptDecision::Deny(reason) => (Value::Null, Some(reason)),
                },
                _ => (args, None),
            };

            let mut ccr_source_original: Option<String> = None;
            // Third field marks a transformed result: its old CCR call is
            // retired, while a source-only lease still protects egress.
            let mut verified_source_for_delivery: Option<(CcrSourceArtifact, String, bool)> = None;
            let (mut content, mut is_error, executed) = match (block_reason, intercept_denial) {
                // Enforce: sensitive tool with tainted args is NOT executed —
                // the structured refusal goes back so the model can re-plan.
                (Some(reason), _) => (reason, true, false),
                // Interceptor refusal — same shape: fed back, never dispatched.
                (None, Some(reason)) => (reason, true, false),
                (None, None) if name == CCR_FIND_TOOL && ccr_find_calls >= CCR_FIND_MAX_CALLS_PER_LOOP => {
                    telemetry.ccr_find_rate_limited =
                        telemetry.ccr_find_rate_limited.saturating_add(1);
                    tracing::warn!(
                        event = "ccr_find",
                        status = "rate_limited",
                        calls = ccr_find_calls,
                        "CCR search budget exhausted for this tool loop"
                    );
                    (CCR_FIND_RATE_LIMITED_MESSAGE.to_string(), true, false)
                }
                (None, None) if name == CCR_FIND_TOOL => {
                    ccr_find_calls = ccr_find_calls.saturating_add(1);
                    if let Some(runtime) = ccr.as_ref() {
                        let query = args
                            .get("query")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_owned();
                        let limit = args
                            .get("limit")
                            .and_then(Value::as_u64)
                            .unwrap_or(5)
                            .min(5) as usize;
                        let runtime_for_read = runtime.clone();
                        match tokio::task::spawn_blocking(move || {
                            runtime_for_read.find_with_status(&query, limit)
                        })
                        .await
                        {
                            Ok(Ok(report)) => {
                                ccr_find_had_hit = !report.hits.is_empty();
                                (
                                    serde_json::to_string(&report)
                                        .expect("CCR report is serializable"),
                                    false,
                                    true,
                                )
                            }
                            Ok(Err(error)) => (error.to_string(), true, true),
                            Err(error) => (format!("CCR search task failed: {error}"), true, true),
                        }
                    } else {
                        (
                            "CCR search is not enabled for this session".into(),
                            true,
                            false,
                        )
                    }
                }
                (None, None) if name == CCR_RETRIEVE_TOOL => {
                    if let Some(runtime) = ccr.as_ref() {
                        let request = (
                            args.get("id").and_then(Value::as_str).map(str::to_owned),
                            args.get("query").and_then(Value::as_str).map(str::to_owned),
                            args.get("offset").and_then(Value::as_u64).unwrap_or(0) as usize,
                            args.get("limit").and_then(Value::as_u64).unwrap_or(8_192) as usize,
                        );
                        if let (Some(retrieve_id), query, offset, limit) = request {
                            let runtime_for_read = runtime.clone();
                            // Logs carry only `sha256(handle)[..8]` — the same digest
                            // `ccr_retrieval_audit` stores. A plaintext handle in the
                            // log would be a second, spec-undocumented audit surface.
                            let audit_id = crate::ccr::handle_log_digest(&retrieve_id);
                            match tokio::task::spawn_blocking(move || {
                                runtime_for_read.retrieve(
                                    &retrieve_id,
                                    query.as_deref(),
                                    offset,
                                    limit,
                                )
                            })
                            .await
                            {
                                Ok(Ok(chunk)) => {
                                    if let Some(guard) = chunk.delivery_guard() {
                                        ccr_delivery_guards.push(guard);
                                    }
                                    ccr_returned_bytes = chunk.text.len() as u64;
                                    tracing::info!(event = "ccr_retrieve", status = "granted", tenant = %runtime.scope.tenant_id, agent = %runtime.scope.agent_id, session = %runtime.scope.session_id, id_sha8 = %audit_id, "CCR retrieval");
                                    (
                                        serde_json::json!({
                                            "text": chunk.text,
                                            "byte_offset": chunk.byte_offset,
                                            "total_bytes": chunk.total_bytes,
                                            "truncated": chunk.truncated
                                        })
                                        .to_string(),
                                        false,
                                        true,
                                    )
                                }
                                Ok(Err(e)) => {
                                    tracing::warn!(event = "ccr_retrieve", status = "refused", tenant = %runtime.scope.tenant_id, agent = %runtime.scope.agent_id, session = %runtime.scope.session_id, id_sha8 = %audit_id, reason = %e, "CCR retrieval refused");
                                    (e.to_string(), true, true)
                                }
                                Err(e) => {
                                    tracing::warn!(event = "ccr_retrieve", status = "failed", tenant = %runtime.scope.tenant_id, agent = %runtime.scope.agent_id, session = %runtime.scope.session_id, id_sha8 = %audit_id, error = %e, "CCR retrieval failed");
                                    (format!("CCR retrieval task failed: {e}"), true, true)
                                }
                            }
                        } else {
                            let store = runtime.store.clone();
                            let scope = runtime.scope.clone();
                            match tokio::task::spawn_blocking(move || {
                                store.record_retrieval_refusal(&scope, "")
                            })
                            .await
                            {
                                Ok(Ok(())) => {}
                                Ok(Err(e)) => {
                                    tracing::warn!(error = %e, "CCR retrieval refusal audit failed")
                                }
                                Err(e) => {
                                    tracing::warn!(error = %e, "CCR retrieval refusal audit task failed")
                                }
                            }
                            tracing::warn!(event = "ccr_retrieve", status = "refused", tenant = %runtime.scope.tenant_id, agent = %runtime.scope.agent_id, session = %runtime.scope.session_id, reason = "missing_id", "CCR retrieval refused");
                            ("CCR retrieval requires a string id".into(), true, true)
                        }
                    } else {
                        tracing::warn!(
                            event = "ccr_retrieve",
                            status = "refused",
                            reason = "disabled",
                            "CCR retrieval refused"
                        );
                        (
                            "CCR retrieval is not enabled for this session".into(),
                            true,
                            false,
                        )
                    }
                }
                (None, None) => match tools.call(&name, args.clone()).await {
                    Ok(outcome) => {
                        let mut source_artifact = outcome.source_artifact;
                        let source_retention_at = outcome.source_retention_at;
                        let mut ccr_eligible = outcome.ccr_eligible;
                        ccr_revoke_call = outcome.ccr_revoke_call;
                        let mut content = outcome.content;
                        let mut result_is_error = outcome.is_error;
                        if source_artifact.is_some() && ccr.is_none() {
                            // A registered source verifier alone cannot keep
                            // a lease through model and channel egress. Do
                            // not release its bytes if the caller omitted
                            // the runtime that owns delivery guards.
                            source_artifact = None;
                            ccr_eligible = false;
                            ccr_revoke_call = true;
                            content =
                                "Trusted MCP source delivery guard unavailable; result withheld"
                                    .into();
                            result_is_error = true;
                        }
                        let pre_redaction_content = content.clone();
                        if let Some(icept) = interceptor.as_ref() {
                            let pretty = content.contains('\n');
                            let mut value = result_to_value(&content);
                            icept.after_call(&server, &name, &args, &mut value);
                            content = value_to_result(value, pretty);
                        }
                        if let Some(artifact) = &source_artifact {
                            if !result_is_error
                                && !tools
                                    .verify_ccr_source(
                                        &name,
                                        &args,
                                        &content,
                                        artifact,
                                        source_retention_at,
                                    )
                                    .await
                            {
                                if content != pre_redaction_content {
                                    // An interceptor may redact a valid source,
                                    // but a source revoked since the first
                                    // attestation must still be withheld.
                                    if tools
                                        .verify_ccr_source(
                                            &name,
                                            &args,
                                            &pre_redaction_content,
                                            artifact,
                                            source_retention_at,
                                        )
                                        .await
                                    {
                                        verified_source_for_delivery = Some((
                                            artifact.clone(),
                                            format!(
                                                "{:x}",
                                                Sha256::digest(pre_redaction_content.as_bytes())
                                            ),
                                            true,
                                        ));
                                        source_artifact = None;
                                        ccr_eligible = false;
                                        ccr_revoke_call = true;
                                    } else {
                                        if let Some(runtime) = ccr.as_ref() {
                                            revoke_observed_stale_artifact(runtime, artifact)
                                                .await?;
                                        }
                                        content =
                                            "Trusted MCP source changed; result withheld".into();
                                        result_is_error = true;
                                    }
                                } else {
                                    if let Some(runtime) = ccr.as_ref() {
                                        revoke_observed_stale_artifact(runtime, artifact).await?;
                                    }
                                    content =
                                        "Trusted MCP source could not be verified; result withheld"
                                            .into();
                                    result_is_error = true;
                                }
                            }
                        }
                        if ccr.is_some() {
                            ccr_source_original = Some(content.clone());
                        }
                        if name != CCR_RETRIEVE_TOOL && name != CCR_FIND_TOOL {
                            if let Some(runtime) = ccr.as_ref() {
                                let source_key = runtime
                                    .source_key_for_call(tools.server_of(&name).as_deref(), &name);
                                let name_lower = name.to_ascii_lowercase();
                                let search_query = if ["search", "grep", "find"]
                                    .iter()
                                    .any(|term| name_lower.contains(term))
                                {
                                    args.get("query")
                                        .or_else(|| args.get("pattern"))
                                        .and_then(Value::as_str)
                                } else {
                                    None
                                };
                                if ccr_eligible
                                    && !result_is_error
                                    && source_key.is_some()
                                    && runtime
                                        .preview_with_query(
                                            &content,
                                            "00000000-0000-4000-8000-000000000000",
                                            search_query,
                                        )
                                        .is_some()
                                {
                                    let store = runtime.store.clone();
                                    let scope = runtime.scope.clone();
                                    let original = content.clone();
                                    let source_tool = source_key.expect("checked source route");
                                    let source_call_id = id.clone();
                                    let source_tool_for_store = source_tool.clone();
                                    let source_call_id_for_store = source_call_id.clone();
                                    let source_artifact_for_store = source_artifact.clone();
                                    match tokio::task::spawn_blocking(move || {
                                        match source_artifact_for_store {
                                            Some(artifact) if source_retention_at.is_some() => {
                                                store.put_bound_until(
                                                    &scope,
                                                    &source_tool_for_store,
                                                    &source_call_id_for_store,
                                                    &original,
                                                    &artifact,
                                                    source_retention_at.expect("checked deadline"),
                                                )
                                            }
                                            Some(artifact) => store.put_bound(
                                                &scope,
                                                &source_tool_for_store,
                                                &source_call_id_for_store,
                                                &original,
                                                &artifact,
                                            ),
                                            None => store.put(
                                                &scope,
                                                &source_tool_for_store,
                                                &source_call_id_for_store,
                                                &original,
                                            ),
                                        }
                                    })
                                    .await
                                    {
                                        Ok(Ok(entry)) => {
                                            let still_valid = match &source_artifact {
                                                Some(artifact) => {
                                                    tools
                                                        .verify_ccr_source(
                                                            &name,
                                                            &args,
                                                            &content,
                                                            artifact,
                                                            source_retention_at,
                                                        )
                                                        .await
                                                }
                                                None => true,
                                            };
                                            if !still_valid {
                                                revoke_observed_stale_artifact(
                                                    runtime,
                                                    source_artifact.as_ref().expect(
                                                        "postcheck applies only to bound source",
                                                    ),
                                                )
                                                .await?;
                                                content =
                                                    "Trusted MCP source changed; result withheld"
                                                        .into();
                                                result_is_error = true;
                                            } else if let Some(preview) = runtime
                                                .preview_with_query(
                                                    &content,
                                                    &entry.id,
                                                    search_query,
                                                )
                                            {
                                                saved_result = Some(CcrSavedResult {
                                                    scope: runtime.scope.clone(),
                                                    source_tool,
                                                    source_call_id,
                                                    id: entry.id,
                                                    original_bytes: content.len(),
                                                    expires_at: entry.expires_at,
                                                });
                                                ccr_preview_sizes =
                                                    Some((content.len(), preview.len()));
                                                content = preview;
                                            }
                                        }
                                        Ok(Err(CcrError::Revoked)) => {
                                            content =
                                                "CCR source call was revoked; result withheld"
                                                    .into();
                                            result_is_error = true;
                                        }
                                        Ok(Err(e)) => {
                                            tracing::warn!(error = %e, "CCR store failed; passing full tool result")
                                        }
                                        Err(e) => {
                                            tracing::warn!(error = %e, "CCR storage task failed; passing full tool result")
                                        }
                                    }
                                }
                            }
                        }
                        if !result_is_error
                            && let (Some(artifact), Some(original)) =
                                (source_artifact, ccr_source_original.as_ref())
                        {
                            verified_source_for_delivery = Some((
                                artifact,
                                format!("{:x}", Sha256::digest(original.as_bytes())),
                                false,
                            ));
                        }
                        (content, result_is_error, true)
                    }
                    // Dispatch failure → feed back as an error result, not a
                    // loop abort, so the model can pick a different tool.
                    Err(reason) => (format!("tool dispatch failed: {reason}"), true, true),
                },
            };

            // The source-call tombstone applies to every dispatched output,
            // including short successes, tool errors, and dispatch failures.
            if executed && name != CCR_RETRIEVE_TOOL && name != CCR_FIND_TOOL {
                if let Some(runtime) = ccr.as_ref() {
                    let store = runtime.store.clone();
                    let scope = runtime.scope.clone();
                    let source_tool = runtime
                        .source_key_for_call(tools.server_of(&name).as_deref(), &name)
                        .unwrap_or_else(|| name.clone());
                    let source_call_id = id.clone();
                    // Reusing a call ID can revoke an earlier saved result.
                    // A returned outcome must not advertise that old handle.
                    ccr_saved_results.retain(|saved| {
                        saved.scope != scope
                            || saved.source_tool != source_tool
                            || saved.source_call_id != source_call_id
                    });
                    let source_original = ccr_source_original;
                    match tokio::task::spawn_blocking(move || {
                        let status = match source_original {
                            Some(original) => store.ensure_source_call_content(
                                &scope,
                                &source_tool,
                                &source_call_id,
                                &original,
                            ),
                            None => store.ensure_source_call_active(
                                &scope,
                                &source_tool,
                                &source_call_id,
                            ),
                        };
                        status?;
                        if ccr_revoke_call {
                            store.revoke_source_call(&scope, &source_tool, &source_call_id)?;
                        }
                        Ok::<(), CcrError>(())
                    })
                    .await
                    {
                        Ok(Ok(())) => {}
                        Ok(Err(CcrError::Revoked)) => {
                            content = "CCR source call was revoked; result withheld".into();
                            is_error = true;
                        }
                        Ok(Err(e)) => {
                            tracing::warn!(error = %e, "CCR source check failed; result withheld");
                            content =
                                "CCR source status could not be verified; result withheld".into();
                            is_error = true;
                        }
                        Err(e) => {
                            tracing::warn!(error = %e, "CCR source check task failed; result withheld");
                            content =
                                "CCR source status could not be verified; result withheld".into();
                            is_error = true;
                        }
                    }
                }
            }

            if !is_error
                && let (Some(runtime), Some((artifact, digest, transformed))) =
                    (ccr.as_ref(), verified_source_for_delivery.as_ref())
            {
                let runtime = runtime.clone();
                let artifact = artifact.clone();
                let digest = digest.clone();
                let transformed = *transformed;
                let source_tool = runtime
                    .source_key_for_call(tools.server_of(&name).as_deref(), &name)
                    .unwrap_or_else(|| name.clone());
                let source_call_id = id.clone();
                match tokio::task::spawn_blocking(move || {
                    if transformed {
                        runtime.acquire_transformed_source_delivery_guard(&artifact, &digest)
                    } else {
                        runtime.acquire_source_delivery_guard(
                            &artifact,
                            &digest,
                            &source_tool,
                            &source_call_id,
                        )
                    }
                })
                .await
                {
                    Ok(Ok(guard)) => ccr_delivery_guards.push(guard),
                    Ok(Err(error)) => {
                        tracing::warn!(error = %error, "CCR source delivery lease refused");
                        content = "Trusted MCP source changed; result withheld".into();
                        is_error = true;
                    }
                    Err(error) => {
                        tracing::warn!(error = %error, "CCR source delivery lease task failed");
                        content =
                            "Trusted MCP source could not be verified; result withheld".into();
                        is_error = true;
                    }
                }
            }

            if is_ccr_find {
                if !is_error && ccr_find_had_hit {
                    telemetry.ccr_find_hits = telemetry.ccr_find_hits.saturating_add(1);
                } else {
                    telemetry.ccr_find_misses = telemetry.ccr_find_misses.saturating_add(1);
                }
            }
            if is_ccr_retrieve {
                if is_error {
                    telemetry.ccr_retrieve_misses = telemetry.ccr_retrieve_misses.saturating_add(1);
                } else {
                    telemetry.ccr_retrieve_successes =
                        telemetry.ccr_retrieve_successes.saturating_add(1);
                    telemetry.ccr_retrieved_bytes = telemetry
                        .ccr_retrieved_bytes
                        .saturating_add(ccr_returned_bytes);
                }
            }
            if !is_error && let Some((raw_bytes, preview_bytes)) = ccr_preview_sizes {
                telemetry.ccr_compressed_results =
                    telemetry.ccr_compressed_results.saturating_add(1);
                telemetry.ccr_original_bytes = telemetry
                    .ccr_original_bytes
                    .saturating_add(raw_bytes as u64);
                telemetry.ccr_delivered_bytes = telemetry
                    .ccr_delivered_bytes
                    .saturating_add(preview_bytes as u64);
            }
            if !is_error && let Some(saved_result) = saved_result {
                ccr_saved_results.push(saved_result);
            }

            // Tool output flows back into the conversation ⇒ register it as a
            // provenance span (Tainted unless the caller vouched for the
            // tool). The synthesized block message is ours — never registered.
            if executed {
                if let Some(ledger) = ledger.as_mut() {
                    let kind = cfg
                        .tool_trust
                        .get(&name)
                        .copied()
                        .unwrap_or(SourceKind::ToolResult);
                    ledger.register(&content, kind);
                }
            }

            // WP-A5: record regardless of `executed` — a provenance-blocked
            // call was still attempted (and refused), which is itself a
            // meaningful signal for the A3 forward model's tool-set/outcome
            // diff, not something to silently drop from the trace. R1: the
            // dispatched/refused call's own text (masked) rides along too.
            tool_calls.push(LoopToolCall {
                tool_name: name.clone(),
                success: !is_error,
                // A retrieved original must not be copied into the audit /
                // grounding event, where no source delivery lease is held.
                result_text: if !is_error
                    && (is_ccr_retrieve || verified_source_for_delivery.is_some())
                {
                    Some("[CCR source result delivered to model after source check]".into())
                } else {
                    mask_and_cap(&content, LOOP_TOOL_CALL_RESULT_MAX_CHARS)
                },
                input_text,
            });

            result_parts.push(ContentPart::ToolResult {
                call_id: id,
                content,
                is_error,
            });
        }
        req.messages.push(ChatMessage {
            role: Role::User,
            parts: result_parts,
        });

        if !ccr_delivery_guards.still_valid().await {
            return Err(LlmError::InvalidRequest(
                "CCR source expired or revoked before model request".into(),
            ));
        }
        last = provider.complete(&req).await?;
        if !ccr_delivery_guards.still_valid().await {
            return Err(LlmError::InvalidRequest(
                "CCR source expired or revoked during model request".into(),
            ));
        }
        telemetry.record_provider(&last);
    }

    // Cap exhausted while still asking for tools: return the last response but
    // flag it so callers don't mistake it for a clean end-of-turn.
    if last.stop == StopReason::ToolUse {
        last.stop = StopReason::Other(MAX_ITERS_STOP.to_string());
    }
    if !ccr_delivery_guards.still_valid().await {
        return Err(LlmError::InvalidRequest(
            "CCR source expired or revoked before delivery".into(),
        ));
    }
    Ok(finish_tool_loop(
        last,
        flags,
        tool_calls,
        ccr_saved_results,
        ccr_delivery_guards,
        telemetry,
        started,
        ccr.as_ref(),
    ))
}

// ---------------------------------------------------------------------------
// PolicyKernel enforcement decorator (P1-4)
// ---------------------------------------------------------------------------
