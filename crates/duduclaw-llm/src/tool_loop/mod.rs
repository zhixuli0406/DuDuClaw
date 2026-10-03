//! Provider-agnostic agentic tool-use loop.
//!
//! When an agent routes to the direct-API path (`openai` / `gemini` /
//! `anthropic` providers) or local inference, it has no CLI backend to broker
//! MCP tools. This module closes that gap: given any [`ChatProvider`] and a
//! [`ToolExecutor`] (the MCP-backed [`crate::ToolRegistry`] in production, a
//! mock in tests), [`run_tool_loop`] drives the model → tool → model cycle
//! until the model stops asking for tools.
//!
//! The loop is deliberately decoupled from MCP: it depends only on the
//! [`ToolExecutor`] trait, so it is fully unit-testable offline without
//! spawning child processes or making HTTP calls.
//!
//! ## Contract
//!
//! 1. If `req.tools` is empty it is seeded from `tools.defs()`; a caller that
//!    pre-populated `req.tools` keeps control.
//! 2. Each turn calls `provider.complete(&req)`. On [`StopReason::ToolUse`]
//!    every [`ContentPart::ToolCall`] is dispatched through the executor, the
//!    assistant turn (verbatim, preserving [`ContentPart::Reasoning`]
//!    signatures for replay) plus a `User` turn of matching
//!    [`ContentPart::ToolResult`] parts are appended, and the loop repeats.
//! 3. Any other stop reason returns the response as-is.
//! 4. **Guard rails.** At most `max_iters` (default via
//!    [`DEFAULT_MAX_TOOL_ITERS`]) tool-dispatch rounds run; on exhaustion the
//!    last response is returned with its stop reason rewritten to
//!    `StopReason::Other("max_tool_iters")`. A per-call executor error is fed
//!    back as a `ToolResult { is_error: true }` so the model can recover — it
//!    never aborts the whole loop (fail-soft for tools, fail-closed only on
//!    provider transport errors, which propagate).

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::sync::{Arc, OnceLock};
use std::time::Instant;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::ccr::{
    CCR_FIND_TOOL, CCR_RETRIEVE_TOOL, CcrError, CcrRuntime, CcrScope, CcrSourceArtifact,
};
use crate::error::LlmError;
use crate::provenance::{
    ProvenanceConfig, ProvenanceFlag, ProvenancePolicy, evaluate_call, seed_default_ledger,
};
#[cfg(test)]
use crate::provenance::SourceKind;
use crate::provider::ChatProvider;
use crate::types::ToolDef;
use crate::types::{ChatMessage, ChatRequest, ChatResponse, ContentPart, Role, StopReason};

/// Default cap on tool-dispatch rounds before the loop gives up.
pub const DEFAULT_MAX_TOOL_ITERS: usize = 10;

/// Stop-reason marker set when the iteration cap is hit.
pub const MAX_ITERS_STOP: &str = "max_tool_iters";

/// Char caps for [`LoopToolCall`]'s masked text fields — reuse the audit
/// trail's own caps (`tool_calls.jsonl`'s `input`/`result_text` fields, and
/// `duduclaw-gateway::runtime::NativeToolEvent`'s identical caps) so a tool
/// call's text is bounded the same regardless of which capture path recorded
/// it.
const LOOP_TOOL_CALL_INPUT_MAX_CHARS: usize = duduclaw_security::audit::AUDIT_INPUT_MAX_CHARS;
const LOOP_TOOL_CALL_RESULT_MAX_CHARS: usize =
    duduclaw_security::audit::AUDIT_RESULT_TEXT_MAX_CHARS;

mod executor;
mod interceptor;
mod loop_run;
mod outcome;
mod telemetry;

#[cfg(test)]
mod tests;

pub use executor::{PolicyExecutor, ToolExecutor};
pub use interceptor::{InterceptDecision, ToolInterceptor};
pub use loop_run::run_tool_loop_with_provenance_and_ccr;
pub use outcome::ToolOutcome;
pub use telemetry::{
    CCR_FIND_MAX_CALLS_PER_LOOP, CcrDeliveryGuards, CcrSavedResult, LoopToolCall, ToolLoopOutcome,
    ToolLoopTelemetry,
};

/// Parse a tool's textual output into the `Value` an interceptor sees.
///
/// JSON in ⇒ JSON out (so structured field rules can address keys); anything
/// else becomes a string leaf (the text rules still run over it).
fn result_to_value(content: &str) -> Value {
    let trimmed = content.trim_start();
    if !trimmed.starts_with('{') && !trimmed.starts_with('[') {
        return Value::String(content.to_string());
    }
    serde_json::from_str::<Value>(content).unwrap_or_else(|_| Value::String(content.to_string()))
}

/// Inverse of [`result_to_value`]. `pretty` mirrors the original's layout so
/// a pretty-printed record dump does not come back as one compact line.
fn value_to_result(value: Value, pretty: bool) -> String {
    match value {
        Value::String(s) => s,
        other => {
            if pretty {
                serde_json::to_string_pretty(&other).unwrap_or_else(|_| other.to_string())
            } else {
                other.to_string()
            }
        }
    }
}

/// A verified connector observed that an immutable source version is no
/// longer current. Tombstone all handles for that version before continuing;
/// a write failure aborts the turn instead of leaving a known-stale handle
/// discoverable after a transient database outage.
async fn revoke_observed_stale_artifact(
    runtime: &CcrRuntime,
    artifact: &CcrSourceArtifact,
) -> Result<(), LlmError> {
    let store = runtime.store.clone();
    let tenant_id = runtime.scope.tenant_id.clone();
    let artifact = artifact.clone();
    match tokio::task::spawn_blocking(move || {
        store.revoke_artifact_version(
            &tenant_id,
            &artifact.connector,
            &artifact.artifact_id,
            &artifact.version,
        )
    })
    .await
    {
        Ok(Ok(_)) => Ok(()),
        Ok(Err(error)) => {
            tracing::error!(error = %error, "CCR source revocation failed");
            Err(LlmError::InvalidRequest(
                "CCR source revocation failed".into(),
            ))
        }
        Err(error) => {
            tracing::error!(error = %error, "CCR source revocation task failed");
            Err(LlmError::InvalidRequest(
                "CCR source revocation task failed".into(),
            ))
        }
    }
}

/// Extract the `(id, name, args)` of every tool call in a response, in order.
fn tool_calls_of(resp: &ChatResponse) -> Vec<(String, String, Value)> {
    resp.parts
        .iter()
        .filter_map(|p| match p {
            ContentPart::ToolCall { id, name, args } => {
                Some((id.clone(), name.clone(), args.clone()))
            }
            _ => None,
        })
        .collect()
}

/// Run the provider-agnostic agentic tool-use loop. See the module docs for
/// the full contract.
///
/// Equivalent to [`run_tool_loop_with_provenance`] with provenance `Off`
/// (zero behavior change — this is the pre-S2 loop).
pub async fn run_tool_loop(
    provider: &dyn ChatProvider,
    req: ChatRequest,
    tools: &dyn ToolExecutor,
    max_iters: usize,
) -> Result<ChatResponse, LlmError> {
    let outcome = run_tool_loop_with_provenance(
        provider,
        req,
        tools,
        max_iters,
        ProvenanceConfig::default(),
        None,
    )
    .await?;
    Ok(outcome.response)
}

/// The tool loop with argument-level provenance tracking (S2 v1 — PACT,
/// arXiv:2605.11039; see [`crate::provenance`] module docs for the model and
/// its honest v1 limits).
///
/// With [`ProvenancePolicy::Off`] (the [`ProvenanceConfig::default`]) the
/// behavior is identical to [`run_tool_loop`]: no ledger is built and no
/// checks run. Otherwise:
///
/// - The ledger starts from `cfg.initial_ledger`, or — fail-safe default —
///   [`seed_default_ledger`] (every pre-existing message part Tainted, only
///   the system prompt trusted).
/// - Before dispatching a call to a tool listed in `cfg.sensitive_tools`,
///   its parsed args are checked; `Warn` records [`ProvenanceFlag`]s and
///   executes, `Enforce` skips execution and feeds back a structured
///   `is_error` tool result so the model can re-plan. Non-sensitive tools
///   always execute. Ledger overflow under `Enforce` blocks sensitive calls
///   fail-closed.
/// - Every *executed* tool's result content is registered back into the
///   ledger under [`ProvenanceConfig::result_trust`] — Tainted
///   (`SourceKind::ToolResult`) unless `cfg.tool_trust` or
///   `cfg.scoped_tool_trust` overrides that call (e.g. a shared-wiki read
///   declared `SourceKind::Wiki` never taints). The loop's own synthesized block
///   message is not registered (it is deterministic and payload-free).
///
/// `interceptor` (RFC-23 §13.6) wraps every dispatch: `before_call` may
/// rewrite the arguments or refuse the call outright (the refusal is fed back
/// as an `is_error` tool result, never dispatched), and `after_call` may
/// rewrite the result text before it re-enters the conversation. `None` ⇒
/// byte-identical to the pre-interceptor loop.
pub async fn run_tool_loop_with_provenance(
    provider: &dyn ChatProvider,
    req: ChatRequest,
    tools: &dyn ToolExecutor,
    max_iters: usize,
    cfg: ProvenanceConfig,
    interceptor: Option<std::sync::Arc<dyn ToolInterceptor>>,
) -> Result<ToolLoopOutcome, LlmError> {
    run_tool_loop_with_provenance_and_ccr(provider, req, tools, max_iters, cfg, interceptor, None)
        .await
}
