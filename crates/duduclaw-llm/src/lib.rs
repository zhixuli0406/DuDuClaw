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
//! - [`FallbackRouter`] — cooldown- and context-window-aware fallback chain
//!   aligned with the gateway rotator semantics (rate-limit 120s, billing
//!   24h, generic 60s).
//! - [`LlmError`] — classified errors with `is_retryable` / `is_failover`,
//!   aligned with the gateway `FailureReason` categories.
//!
//! Gateway integration (credential resolution, account rotation, telemetry
//! wiring) is a later wave; this crate is deliberately gateway-free.

mod error;
mod http;
mod provider;
mod registry;
mod router;
mod sse;
mod tool_loop;
mod types;

#[cfg(feature = "mcp-client")]
mod mcp_client;

pub mod providers;

pub use error::{classify_http, classify_transport, LlmError};
pub use provider::{resolve_env_key, split_model_id, ApiAuth, ChatProvider, ProviderId};
pub use registry::{Feature, ModelCaps, ModelInfo, ModelRegistry, PriceCliff};
pub use router::{
    cooldown_for, CandidateOutcome, FallbackRouter, BILLING_COOLDOWN, GENERIC_COOLDOWN,
    RATE_LIMIT_COOLDOWN,
};
pub use tool_loop::{
    run_tool_loop, PolicyExecutor, ToolExecutor, ToolOutcome, DEFAULT_MAX_TOOL_ITERS,
    MAX_ITERS_STOP,
};

#[cfg(feature = "mcp-client")]
pub use mcp_client::{
    build_initialize_request, build_initialized_notification, build_tools_call_request,
    build_tools_list_request, parse_tool_call_result, parse_tools_list_response, McpClient,
    McpError, McpToolDef, ToolCallResult, ToolRegistry, DEFAULT_MCP_TIMEOUT,
};
pub use types::{
    estimate_tokens, CacheHint, ChatMessage, ChatRequest, ChatResponse, ContentPart,
    NormalizedUsage, ReasoningHint, Role, StopReason, StreamEvent, SystemBlock, ToolChoice,
    ToolDef, CACHE_SPLIT_MARKER,
};
