use super::*;

/// Extract a human-readable detail from a `tool_use` content block's `input`.
///
/// Tries common field names: `file_path`, `path`, `command`, `pattern`, `query`.
/// Returns the first match (truncated to 60 chars for display).
pub(crate) fn extract_tool_detail(block: &serde_json::Value) -> Option<String> {
    let input = block.get("input")?;
    for key in &["file_path", "path", "command", "pattern", "query"] {
        if let Some(val) = input.get(key).and_then(|v| v.as_str()) {
            let truncated: String = val.chars().take(60).collect();
            return Some(truncated);
        }
    }
    None
}

// ── Direct API delegate (Rust-native) ───────────────────────

/// Synchronous delegation helper: call the Anthropic Messages API directly
/// using the configured API key. Replaces the former Python SDK subprocess
/// bridge (`duduclaw.sdk.chat`) so the gateway has no runtime Python
/// dependency. Returns an error when no API key is configured — OAuth-only
/// setups should delegate via the CLI/PTY path instead.
pub async fn call_direct_api_delegate(
    prompt: &str,
    model: &str,
    system_prompt: &str,
    home_dir: &Path,
) -> Result<String, String> {
    let api_key = get_api_key(home_dir)
        .await
        .ok_or_else(|| "No API key configured for Direct API delegation".to_string())?;
    let resp =
        crate::direct_api::call_direct_api(&api_key, model, system_prompt, prompt, &[]).await?;
    Ok(resp.text)
}

/// P34 #4: give a `system_operator`-capable agent real tool-calling ability
/// on the user-facing Direct-API fallback (the branch above always sends an
/// empty tool list, so an operator's `os_intent`/`os_operator` Guide hint —
/// see `operator_guide_hint` further up this file — could never actually be
/// acted on when a turn landed here: `call_direct_api`/`call_direct_api_attributed`
/// (this file) build a hand-rolled Anthropic Messages request with no `tools`
/// field at all, and the *dispatcher-side* Direct-API path
/// (`claude_runner::try_direct_api`) has the identical limitation for
/// Anthropic models — it explicitly keeps this file's tools-less handler for
/// its cache attribution and only routes non-Anthropic providers through the
/// MCP tool loop. `duduclaw_llm::providers::AnthropicProvider` already
/// implements the tool-capable `ChatProvider` trait — it was written to
/// "absorb the cache-placement behavior of ... `direct_api.rs`" (see that
/// module's doc comment) for exactly this kind of caller — but had zero
/// production call sites anywhere in the gateway before this. The wiring
/// below mirrors the already-proven pattern used by
/// `runtime/openai_compat.rs::execute_with_tools` and
/// `local_llm::try_local_tool_loop`: build an MCP `ToolRegistry`, apply the
/// same fail-closed capability filter (G2) and static `PolicyKernel` policy
/// (I3) every other tool-loop caller applies, then drive
/// `duduclaw_llm::run_tool_loop_with_provenance`.
///
/// Returns `Some(text)` only on a successful, non-empty tool-loop answer.
/// Every other outcome returns `None` so the caller falls back to the plain
/// `call_direct_api` call exactly as it did before this function existed
/// (fail-safe, same contract as `local_llm::try_local_tool_loop`):
/// - the MCP registry failed to spawn/handshake/list;
/// - the capability filter left no tools (fail-closed — never re-seeds from
///   the unfiltered registry);
/// - the loop itself errored, or finished with empty answer text.
///
/// Gate: the ONLY caller is the `system_operator` branch inside
/// `build_reply_with_session_inner`'s Direct-API fallback — every other
/// agent's Direct-API path never calls this function, so its behavior is
/// byte-identical to before. The O-0/O-4 MCP dispatch gates, capability
/// scopes, and audit trail are unchanged: every dispatched call still goes
/// through the same spawned `duduclaw mcp-server` subprocess and its
/// existing enforcement, exactly as the CLI path already does — this
/// function only decides whether a tool CAN be offered to the model, never
/// whether a call is allowed to execute.
///
/// Build the normalized [`duduclaw_llm::ChatRequest`] for the operator tool
/// loop: system prompt segmented on the same cache-breakpoint marker as this
/// file's tools-less path (`split_system_segments`), then the current user
/// message, then the pre-filtered tool defs. Pure and I/O-free — extracted
/// out of `try_operator_direct_api_tool_loop` (which also spawns an MCP
/// subprocess and makes the HTTP call) purely so this piece is directly
/// unit-testable, mirroring `runtime/openai_compat.rs::build_tool_chat_request`
/// and `local_llm.rs::flatten_chat_request`.
pub(super) fn build_operator_tool_chat_request(
    model: &str,
    system_prompt: &str,
    user_prompt: &str,
    tools: Vec<duduclaw_llm::ToolDef>,
) -> duduclaw_llm::ChatRequest {
    let mut req = duduclaw_llm::ChatRequest::new(model.to_string());
    for seg in crate::direct_api::split_system_segments(system_prompt) {
        req.system.push(duduclaw_llm::SystemBlock::cached(seg));
    }
    req.messages
        .push(duduclaw_llm::ChatMessage::user(user_prompt.to_string()));
    req.tools = tools;
    req
}

pub(super) async fn try_operator_direct_api_tool_loop(
    agent_id: &str,
    api_key: &str,
    model: &str,
    system_prompt: &str,
    user_prompt: &str,
    capabilities: Option<&duduclaw_core::types::CapabilitiesConfig>,
) -> Option<String> {
    // Phase A — MCP tool registry (fail-safe: any spawn/handshake/list
    // failure ⇒ None ⇒ caller degrades to the plain tools-less call).
    let registry = crate::claude_runner::build_mcp_tool_registry(agent_id).await?;
    let tools = crate::claude_runner::filter_tool_defs(registry.tool_defs(), capabilities);
    if tools.is_empty() {
        info!(
            agent = %agent_id,
            "operator Direct-API tool loop skipped — capability filter left no tools"
        );
        return None;
    }

    let auth = duduclaw_llm::ApiAuth::new(api_key.to_string());
    let provider = duduclaw_llm::providers::AnthropicProvider::new(auth);

    let req = build_operator_tool_chat_request(model, system_prompt, user_prompt, tools);

    // Fail-closed capability filter already ran above; this is the static
    // PolicyKernel layer (complete mediation, I3) every other tool-loop
    // caller applies on top of it. Empty policy ⇒ the kernel abstains
    // (passthrough) — byte-identical to no policy.
    let empty_policy: Vec<duduclaw_core::types::ToolPolicy> = Vec::new();
    let policy = capabilities
        .map(|c| c.policy.as_slice())
        .unwrap_or(&empty_policy);
    let guarded = duduclaw_llm::PolicyExecutor::new(&registry, policy, agent_id);

    // RFC-23 §13.6: this loop dispatches MCP tools in-process, so nothing
    // upstream redacts their results. Redaction inactive ⇒ `None` ⇒
    // byte-identical. Enabled-but-broken ⇒ skip the tool loop entirely
    // (fail-closed: the caller degrades to the tools-less call rather than
    // feeding the model unredacted tool output).
    let interceptor = match crate::redaction_proxy::try_build_interceptor(
        &duduclaw_core::duduclaw_home(),
        agent_id,
        &crate::redaction_proxy::current_session_id(),
    ) {
        Ok(i) => i.map(|i| i as std::sync::Arc<dyn duduclaw_llm::ToolInterceptor>),
        Err(e) => {
            warn!(
                agent = %agent_id, error = %e,
                "operator Direct-API tool loop skipped — redaction is enabled but failed to initialise"
            );
            return None;
        }
    };

    let loop_result = duduclaw_llm::run_tool_loop_with_provenance_and_ccr(
        &provider,
        req,
        &guarded,
        duduclaw_llm::DEFAULT_MAX_TOOL_ITERS,
        duduclaw_llm::ProvenanceConfig::default(),
        interceptor,
        crate::ccr_runtime::for_agent(&duduclaw_core::duduclaw_home(), agent_id),
    )
    .await;

    match loop_result {
        Ok(outcome) => {
            let text = outcome.response.text();
            if text.trim().is_empty() {
                warn!(
                    agent = %agent_id,
                    stop = ?outcome.response.stop,
                    "operator Direct-API tool loop returned empty text — falling back to plain call"
                );
                return None;
            }
            if !crate::ccr_runtime::capture_delivery_guards(outcome.ccr_delivery_guards).await {
                warn!(agent = %agent_id, "operator CCR delivery guard could not be retained");
                return Some(CCR_DELIVERY_REFUSED_TEXT.to_string());
            }
            crate::ccr_runtime::capture_saved_results(outcome.ccr_saved_results);
            // R1 parity: feed the same best-effort NATIVE_TOOL_COLLECTOR sink
            // every other tool-loop producer uses (openai-compat / local /
            // claude_runner's non-Anthropic Direct-API path) — a Guide-path
            // result card can render whenever the caller happens to be
            // scoped, with the identical caveat as those paths: a silent
            // no-op outside a scoped caller, never a failure.
            crate::runtime::extend_native_tool_events(
                outcome
                    .tool_calls
                    .into_iter()
                    .map(|c| crate::runtime::NativeToolEvent {
                        tool_name: c.tool_name,
                        success: c.success,
                        result_text: c.result_text,
                        input_text: c.input_text,
                    })
                    .collect(),
            );
            info!(
                agent = %agent_id,
                model,
                "operator answered Direct-API fallback via MCP tool loop"
            );
            Some(text)
        }
        Err(e) => {
            warn!(
                agent = %agent_id,
                error = %e,
                "operator Direct-API tool loop failed — falling back to plain call"
            );
            None
        }
    }
}

