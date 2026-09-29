//! # duduclaw-llm — provider-agnostic completion layer
//!
//! The API-level twin of the CLI-level `AgentRuntime` trait: one normalized
//! request/response shape ([`ChatRequest`] / [`ChatResponse`], Vercel AI SDK
//! v5 content-parts with Anthropic semantics), a [`ChatProvider`] trait, and
//! four providers:
//!
//! - **Anthropic** (Messages API) — layered `cache_control` breakpoints
//!   (≤ 3 system + the "system_and_3" history breakpoint, absorbed from the
//!   gateway's `direct_api.rs`), thinking replay, real SSE streaming.
//! - **OpenAI** (Responses API — not the sunsetting Chat Completions) —
//!   `input` items, `reasoning.effort`, buffered streaming in v1.
//! - **Gemini** (native `generateContent`) — `thoughtSignature` echoed back
//!   verbatim, `functionCallingConfig` modes, buffered streaming in v1.
//! - **OpenAI-compat** (legacy `chat/completions`) — DeepSeek/Qwen/xAI/Groq/
//!   Together/Mistral/MiniMax/OpenRouter/local presets, string tool-args
//!   parsed at the boundary, reasoning_content support, real SSE streaming.
//!
//! Plus:
//! - [`ModelRegistry`] — vendored model table (context windows, millicent
//!   pricing with price cliffs, capability flags) + user override loader.
//! - MoA virtual models ([`complete_moa_model`]) — named `moa:<name>`
//!   ensembles from `[moa.<name>]` override sections: parallel proposer
//!   opinion pass + a single tool-calling aggregator, fail-closed on
//!   unknown names.
//! - [`LlmError`] — classified errors with `is_retryable` / `is_failover`,
//!   aligned with the gateway `FailureReason` categories.
//!
//! Gateway integration (credential resolution, account rotation, telemetry
//! wiring) is a later wave; this crate is deliberately gateway-free.

mod ccr;
mod error;
mod http;
mod moa;
mod provenance;
mod provider;
mod registry;
mod sse;
mod tool_loop;
mod types;

#[cfg(feature = "mcp-client")]
mod mcp_client;

pub mod providers;

pub use ccr::{
    CCR_FIND_TOOL, CCR_LOOP_TELEMETRY_MAX_ROWS, CCR_RETRIEVE_TOOL, CcrBoundSourceValidator,
    CcrDeliveryLease, CcrEntry, CcrError, CcrFindHit, CcrFindReport, CcrRuntime, CcrScope,
    CcrSourceArtifact, CcrStore, RetrievedChunk,
};
pub use error::{LlmError, classify_http, classify_transport};
pub use moa::{
    DEFAULT_PROPOSER_MAX_TOKENS, MOA_MODEL_PREFIX, MoaResponse, MoaSpec, complete_moa,
    complete_moa_model, is_moa_model_id, moa_name, stream_moa, stream_moa_model,
};
pub use provenance::{
    CJK_TAINT_MIN_CHARS, CallDecision, DEFAULT_TAINT_MIN_CHARS, FlagKind, MAX_LEDGER_SPANS,
    MAX_LEDGER_TOTAL_CHARS, PREVIEW_MAX_CHARS, ProvenanceConfig, ProvenanceFlag, ProvenanceLedger,
    ProvenancePolicy, SensitiveTool, SourceKind, TaintHit, TrustLevel, evaluate_call,
    seed_default_ledger,
};
pub use provider::{ApiAuth, ChatProvider, ProviderId, resolve_env_key, split_model_id};
pub use registry::{Feature, ModelCaps, ModelInfo, ModelRegistry, PriceCliff};
pub use tool_loop::{
    CcrDeliveryGuards, CcrSavedResult, DEFAULT_MAX_TOOL_ITERS, InterceptDecision, MAX_ITERS_STOP,
    PolicyExecutor, ToolExecutor, ToolInterceptor, ToolLoopOutcome, ToolLoopTelemetry, ToolOutcome,
    run_tool_loop, run_tool_loop_with_provenance, run_tool_loop_with_provenance_and_ccr,
};

#[cfg(feature = "mcp-client")]
pub use mcp_client::{
    DEFAULT_MCP_TIMEOUT, McpClient, McpError, McpSourceVerifier, McpToolDef, ToolCallResult,
    ToolFilter, ToolRegistry, VerifiedMcpSource, build_initialize_request,
    build_initialized_notification, build_tools_call_request, build_tools_list_request,
    parse_tool_call_result, parse_tools_list_response,
};
pub use types::{
    CACHE_SPLIT_MARKER, CacheHint, ChatMessage, ChatRequest, ChatResponse, ContentPart,
    NormalizedUsage, ReasoningHint, Role, StopReason, StreamEvent, SystemBlock, ToolChoice,
    ToolDef, estimate_tokens,
};
