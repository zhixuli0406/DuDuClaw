//! `LocalChatProvider` — bridges the local inference stack
//! (`duduclaw-inference`: llamafile / Exo / vLLM / llama.cpp / mistral.rs)
//! into the [`duduclaw_llm::ChatProvider`] trait so
//! [`duduclaw_llm::run_tool_loop`] can drive it with MCP tools.
//!
//! Design decision (approved 2026-07): `duduclaw-inference` and
//! `duduclaw-llm` stay decoupled — the adapter lives HERE in the gateway,
//! which already depends on both crates.
//!
//! ## Delegation strategy
//!
//! **(a) OpenAI-compatible HTTP endpoint** (the common case — llamafile, Exo,
//! vLLM, SGLang, or a configured `[openai_compat]` server): delegate to
//! [`duduclaw_llm::providers::OpenAiCompatProvider`] pointed at the engine's
//! base URL. Chosen as the primary strategy because it inherits the
//! battle-tested chat/completions translation for free — tool-call JSON
//! encode/decode with string-argument parsing at the boundary,
//! `finish_reason` → [`duduclaw_llm::StopReason`] mapping, and real SSE —
//! rather than re-implementing a second, drift-prone tool-call codec against
//! the raw `InferenceEngine` request shape. Tool capability additionally
//! requires the `inference.toml [router] local_tools` gate (default **true**;
//! small models may emit malformed tool calls, which the tool loop already
//! feeds back fail-soft).
//!
//! **(b) In-process backend** (llama.cpp / mistral.rs — no HTTP surface):
//! `complete()` flattens the [`ChatRequest`] onto the engine's system/user
//! prompt API. Tool calling is NOT supported there (`supports_tools() =
//! false`); callers skip the tool loop and get a bare completion — exactly
//! today's behavior.
//!
//! ## Fail-safe contract
//!
//! Every entry point degrades instead of erroring the reply path:
//! `from_engine` returns `None` when local inference is unavailable, and
//! [`try_local_tool_loop`] returns `None` on any failure (no registry, empty
//! filtered tool set, loop error, empty text) so callers fall back to the
//! bare `call_local_inference` path unchanged.

use std::sync::Arc;

use async_trait::async_trait;
use futures_util::stream::BoxStream;
use tracing::{info, warn};

use duduclaw_inference::InferenceEngine;
use duduclaw_llm::providers::OpenAiCompatProvider;
use duduclaw_llm::{
    ApiAuth, ChatMessage, ChatProvider, ChatRequest, ChatResponse, ContentPart, LlmError,
    NormalizedUsage, Role, StopReason, StreamEvent, SystemBlock,
};

/// Provider id reported by the adapter (telemetry / logs).
const LOCAL_PROVIDER_ID: &str = "local";

enum Inner {
    /// Strategy (a): OpenAI-compatible HTTP endpoint — full tool-call codec.
    Compat(OpenAiCompatProvider),
    /// Strategy (b): in-process engine — flattened prompt, no tools.
    Engine(Arc<InferenceEngine>),
}

/// [`ChatProvider`] over the local inference stack. Construct via
/// [`LocalChatProvider::from_engine`].
pub struct LocalChatProvider {
    inner: Inner,
    /// Model the endpoint/engine expects (fallback when the request's model
    /// id is empty).
    model: String,
    tools_capable: bool,
}

impl LocalChatProvider {
    /// Build a provider over the engine's active backend.
    ///
    /// Returns `None` when local inference is unavailable (disabled, or no
    /// backend initialized). The `bool` is `tools_capable`: `true` only for
    /// an OpenAI-compat endpoint with `[router] local_tools` enabled
    /// (default true) — callers must skip the tool loop otherwise.
    pub async fn from_engine(engine: &Arc<InferenceEngine>) -> Option<(Self, bool)> {
        if let Some(ep) = engine.compat_endpoint().await {
            let tools_capable = tools_capability(true, engine.local_tools_enabled());
            // Empty key ⇒ OpenAiCompatProvider sends no Authorization header
            // (keyless local servers), matching the engine's own behavior.
            let auth = ApiAuth::new(ep.api_key.unwrap_or_default());
            let provider = OpenAiCompatProvider::new(LOCAL_PROVIDER_ID, auth, ep.base_url);
            return Some((
                Self { inner: Inner::Compat(provider), model: ep.model, tools_capable },
                tools_capable,
            ));
        }
        // In-process backend (llama.cpp / mistral.rs): cheap non-HTTP check.
        if engine.is_available().await {
            let model = engine.config().default_model.clone().unwrap_or_default();
            return Some((
                Self { inner: Inner::Engine(engine.clone()), model, tools_capable: false },
                false,
            ));
        }
        None
    }

    /// Whether this provider can be driven by the tool loop.
    pub fn supports_tools(&self) -> bool {
        self.tools_capable
    }

    /// Model the local endpoint expects (request fallback).
    pub fn model(&self) -> &str {
        &self.model
    }
}

/// Pure tools-capability decision: an OpenAI-compat endpoint must exist AND
/// the `[router] local_tools` gate must allow it. In-process backends are
/// never tools-capable (no tool-call wire format).
fn tools_capability(has_compat_endpoint: bool, local_tools_enabled: bool) -> bool {
    has_compat_endpoint && local_tools_enabled
}

/// Flatten a [`ChatRequest`] onto the in-process engine's
/// `(system_prompt, user_prompt)` shape — strategy (b), pure.
///
/// System blocks join with blank lines. A single message passes its text
/// through verbatim (today's bare-completion shape); a multi-message
/// conversation becomes a role-labeled transcript. Non-text parts (tool
/// calls/results, images, reasoning) are dropped — they can only appear on
/// the tool-loop path, which never reaches this backend.
fn flatten_chat_request(req: &ChatRequest) -> (String, String) {
    let system = req
        .system
        .iter()
        .map(|b| b.text.as_str())
        .collect::<Vec<_>>()
        .join("\n\n");

    let turns: Vec<(Role, String)> = req
        .messages
        .iter()
        .map(|m| {
            let text = m
                .parts
                .iter()
                .filter_map(|p| match p {
                    ContentPart::Text(t) => Some(t.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n");
            (m.role, text)
        })
        .filter(|(_, t)| !t.is_empty())
        .collect();

    let user = if turns.len() <= 1 {
        turns.into_iter().map(|(_, t)| t).next().unwrap_or_default()
    } else {
        turns
            .into_iter()
            .map(|(role, t)| match role {
                Role::User => format!("User: {t}"),
                Role::Assistant => format!("Assistant: {t}"),
            })
            .collect::<Vec<_>>()
            .join("\n\n")
    };
    (system, user)
}

#[async_trait]
impl ChatProvider for LocalChatProvider {
    fn id(&self) -> &str {
        LOCAL_PROVIDER_ID
    }

    async fn complete(&self, req: &ChatRequest) -> Result<ChatResponse, LlmError> {
        match &self.inner {
            Inner::Compat(p) => p.complete(req).await,
            Inner::Engine(engine) => {
                let (system, user) = flatten_chat_request(req);
                let (_, bare_model) = duduclaw_llm::split_model_id(&req.model);
                let request = duduclaw_inference::InferenceRequest {
                    system_prompt: system,
                    user_prompt: user,
                    params: engine.config().generation.clone(),
                    model_id: if bare_model.trim().is_empty() {
                        None
                    } else {
                        Some(bare_model.to_string())
                    },
                };
                // Backend hiccups classify as Network: retryable/failover for
                // the caller's fallback chain, never a hard reply failure.
                let resp = engine
                    .generate(&request)
                    .await
                    .map_err(|e| LlmError::Network(format!("local inference: {e}")))?;
                Ok(ChatResponse {
                    parts: vec![ContentPart::Text(resp.text)],
                    stop: StopReason::EndTurn,
                    usage: NormalizedUsage {
                        input_tokens: resp.tokens_prompt as u64,
                        output_tokens: resp.tokens_generated as u64,
                        ..Default::default()
                    },
                    model_used: resp.model_id,
                    provider: LOCAL_PROVIDER_ID.to_string(),
                })
            }
        }
    }

    async fn stream(
        &self,
        req: &ChatRequest,
    ) -> Result<BoxStream<'static, Result<StreamEvent, LlmError>>, LlmError> {
        match &self.inner {
            // Real SSE for HTTP endpoints.
            Inner::Compat(p) => p.stream(req).await,
            // Buffered: complete() then one Done event.
            Inner::Engine(_) => {
                let resp = self.complete(req).await?;
                Ok(Box::pin(futures_util::stream::once(async move {
                    Ok(StreamEvent::Done(resp))
                })))
            }
        }
    }
}

/// Run the MCP tool loop against the local OpenAI-compat endpoint.
///
/// Returns `Some(text)` only on a successful, non-empty tool-loop answer.
/// Every other outcome returns `None` so the caller falls back to the bare
/// completion path exactly as today (fail-safe):
/// - local backend unavailable or not tools-capable (in-process backend, or
///   `[router] local_tools = false`);
/// - MCP registry spawn/list failure;
/// - the capability filter removed every tool (fail-closed — `run_tool_loop`
///   would otherwise re-seed `req.tools` from the *unfiltered* registry);
/// - the loop errored or produced empty text.
pub(crate) async fn try_local_tool_loop(
    engine: &Arc<InferenceEngine>,
    prompt: &str,
    system_prompt: &str,
    model_id: Option<&str>,
    agent_id: &str,
    capabilities: Option<&duduclaw_core::types::CapabilitiesConfig>,
) -> Option<String> {
    let (provider, tools_capable) = LocalChatProvider::from_engine(engine).await?;
    if !tools_capable {
        return None;
    }
    let registry = crate::claude_runner::build_mcp_tool_registry(agent_id).await?;
    let tools = crate::claude_runner::filter_tool_defs(registry.tool_defs(), capabilities);
    if tools.is_empty() {
        // Fail-closed: never let the loop re-seed tools the filter removed.
        info!(agent = %agent_id, "local tool loop skipped — capability filter left no tools");
        return None;
    }

    let model = model_id
        .filter(|m| !m.trim().is_empty())
        .unwrap_or_else(|| provider.model())
        .to_string();
    let mut req = ChatRequest::new(model);
    let system = system_prompt.trim();
    if !system.is_empty() {
        // Local servers (vLLM APC / SGLang RadixAttention) prefix-cache
        // implicitly; no explicit breakpoint needed.
        req.system.push(SystemBlock::uncached(system));
    }
    req.messages.push(ChatMessage::user(prompt));
    req.tools = tools;

    match duduclaw_llm::run_tool_loop(
        &provider,
        req,
        &registry,
        duduclaw_llm::DEFAULT_MAX_TOOL_ITERS,
    )
    .await
    {
        Ok(resp) => {
            let text = resp.text();
            if text.trim().is_empty() {
                warn!(
                    agent = %agent_id,
                    stop = ?resp.stop,
                    "local tool loop returned empty text — falling back to bare completion"
                );
                None
            } else {
                info!(
                    agent = %agent_id,
                    model = %resp.model_used,
                    "local inference answered via MCP tool loop"
                );
                Some(text)
            }
        }
        Err(e) => {
            warn!(
                agent = %agent_id,
                error = %e,
                "local tool loop failed — falling back to bare completion"
            );
            None
        }
    }
}

// ---------------------------------------------------------------------------
// Tests — offline: no HTTP, no processes, no model files.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // ── tools_capability decision (pure, table-driven) ─────────────────

    #[test]
    fn tools_capability_requires_compat_endpoint_and_gate() {
        // (has_compat_endpoint, local_tools_enabled) → tools_capable
        for (compat, gate, expected) in [
            (true, true, true),    // compat URL + gate on → tools
            (true, false, false),  // operator disabled local_tools
            (false, true, false),  // in-process backend never tool-capable
            (false, false, false),
        ] {
            assert_eq!(tools_capability(compat, gate), expected, "({compat}, {gate})");
        }
    }

    // ── flatten_chat_request (strategy b, pure) ────────────────────────

    #[test]
    fn flatten_single_message_passes_text_verbatim() {
        let mut req = ChatRequest::new("m");
        req.system.push(SystemBlock::cached("rules"));
        req.system.push(SystemBlock::uncached("queue"));
        req.messages.push(ChatMessage::user("哈囉 hello"));
        let (system, user) = flatten_chat_request(&req);
        assert_eq!(system, "rules\n\nqueue");
        // Single turn: no role label (today's bare-completion shape).
        assert_eq!(user, "哈囉 hello");
    }

    #[test]
    fn flatten_multi_turn_labels_roles() {
        let mut req = ChatRequest::new("m");
        req.messages.push(ChatMessage::user("question"));
        req.messages.push(ChatMessage::assistant("answer"));
        req.messages.push(ChatMessage::user("follow-up"));
        let (system, user) = flatten_chat_request(&req);
        assert_eq!(system, "");
        assert_eq!(user, "User: question\n\nAssistant: answer\n\nUser: follow-up");
    }

    #[test]
    fn flatten_drops_non_text_parts() {
        let mut req = ChatRequest::new("m");
        req.messages.push(ChatMessage {
            role: Role::User,
            parts: vec![
                ContentPart::Text("look".into()),
                ContentPart::Image {
                    media_type: "image/png".into(),
                    data_base64: "aGk=".into(),
                },
                ContentPart::ToolResult {
                    call_id: "c1".into(),
                    content: "res".into(),
                    is_error: false,
                },
            ],
        });
        let (_, user) = flatten_chat_request(&req);
        assert_eq!(user, "look");
    }

    #[test]
    fn flatten_empty_request_is_empty() {
        let req = ChatRequest::new("m");
        assert_eq!(flatten_chat_request(&req), (String::new(), String::new()));
    }

    // ── from_engine ────────────────────────────────────────────────────

    async fn engine_with_toml(toml_str: &str) -> (tempfile::TempDir, Arc<InferenceEngine>) {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        std::fs::write(tmp.path().join("inference.toml"), toml_str).expect("write toml");
        let engine = Arc::new(InferenceEngine::new(tmp.path()).await);
        (tmp, engine)
    }

    #[tokio::test]
    async fn from_engine_none_when_unavailable() {
        // enabled = false (default config) → no provider at all.
        let (_tmp, engine) = engine_with_toml("enabled = false").await;
        assert!(LocalChatProvider::from_engine(&engine).await.is_none());
        // enabled but no backend initialized (no compat config, no init()).
        let (_tmp2, engine2) = engine_with_toml("enabled = true").await;
        assert!(LocalChatProvider::from_engine(&engine2).await.is_none());
    }

    #[tokio::test]
    async fn from_engine_compat_endpoint_is_tools_capable_by_default() {
        let (_tmp, engine) = engine_with_toml(
            r#"
enabled = true

[openai_compat]
base_url = "http://localhost:8080/v1"
model = "qwen3-8b"
"#,
        )
        .await;
        let (provider, tools_capable) = LocalChatProvider::from_engine(&engine)
            .await
            .expect("compat endpoint present");
        assert!(tools_capable, "local_tools defaults to enabled for compat backends");
        assert!(provider.supports_tools());
        assert_eq!(provider.model(), "qwen3-8b");
        assert_eq!(provider.id(), "local");
        assert!(matches!(provider.inner, Inner::Compat(_)));
    }

    #[tokio::test]
    async fn from_engine_respects_local_tools_gate() {
        let (_tmp, engine) = engine_with_toml(
            r#"
enabled = true

[openai_compat]
base_url = "http://localhost:8080/v1"
model = "qwen3-8b"

[router]
local_tools = false
"#,
        )
        .await;
        let (provider, tools_capable) = LocalChatProvider::from_engine(&engine)
            .await
            .expect("compat endpoint present");
        assert!(!tools_capable, "[router] local_tools = false must disable the tool loop");
        assert!(!provider.supports_tools());
        // Delegation still uses the compat client for bare completions.
        assert!(matches!(provider.inner, Inner::Compat(_)));
    }
}
