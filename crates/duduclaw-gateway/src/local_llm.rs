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
    NormalizedUsage, Role, StopReason, StreamEvent, SystemBlock, ToolDef,
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

    /// Base URL of the OpenAI-compatible endpoint this provider talks to
    /// (`None` for the in-process engine path).
    pub fn compat_base_url(&self) -> Option<&str> {
        match &self.inner {
            Inner::Compat(p) => Some(p.base_url()),
            Inner::Engine(_) => None,
        }
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

/// Tokens kept free for the model's own output and tool-call round trips
/// when fitting a request into a known context window.
const LOCAL_CTX_RESERVE_TOKENS: u64 = 1024;
/// Share of the remaining budget the tool definitions may take; the rest
/// goes to the system prompt.
const LOCAL_CTX_TOOL_SHARE: f64 = 0.45;
/// Tools the goal loop's work message asks the agent to call by name; kept
/// even when everything else has to go.
const LOCAL_CTX_ALWAYS_KEEP_PREFIX: &str = "tasks_";
const LOCAL_CTX_TRIM_MARKER: &str = "\n\n[…系統提示已依本地模型的 context 長度截短…]";

/// Result of [`fit_request_to_context`].
#[derive(Debug)]
struct FittedRequest {
    system_prompt: String,
    tools: Vec<ToolDef>,
    trimmed_tools: usize,
    trimmed_system_chars: usize,
}

fn tool_tokens(t: &ToolDef) -> u64 {
    crate::prompt_compression::estimate_tokens(&t.name)
        + crate::prompt_compression::estimate_tokens(&t.description)
        + crate::prompt_compression::estimate_tokens(&t.input_schema.to_string())
        + 8
}

/// Pure: trim `tools` and `system_prompt` so that system + tools + `prompt`
/// + [`LOCAL_CTX_RESERVE_TOKENS`] fit in `n_ctx` (estimated tokens, CJK-aware).
/// Tools are kept in registry order; `tasks_*` tools are always kept
/// (the goal-loop work message names them). The system prompt is cut at
/// a char boundary with a visible marker rather than silently.
fn fit_request_to_context(
    system_prompt: &str,
    prompt: &str,
    tools: Vec<ToolDef>,
    n_ctx: u64,
) -> FittedRequest {
    let est = crate::prompt_compression::estimate_tokens;
    let budget = n_ctx.saturating_sub(LOCAL_CTX_RESERVE_TOKENS).saturating_sub(est(prompt));
    let system_tokens = est(system_prompt);
    let tools_total: u64 = tools.iter().map(tool_tokens).sum();
    if system_tokens + tools_total <= budget {
        return FittedRequest {
            system_prompt: system_prompt.to_string(),
            tools,
            trimmed_tools: 0,
            trimmed_system_chars: 0,
        };
    }

    let tool_budget = (budget as f64 * LOCAL_CTX_TOOL_SHARE) as u64;
    let original_tools = tools.len();
    let mut kept: Vec<ToolDef> = Vec::new();
    let mut used: u64 = 0;
    for t in tools {
        let cost = tool_tokens(&t);
        let must_keep = t.name.starts_with(LOCAL_CTX_ALWAYS_KEEP_PREFIX);
        if must_keep || used + cost <= tool_budget {
            used += cost;
            kept.push(t);
        }
    }
    let trimmed_tools = original_tools - kept.len();

    let system_budget = budget.saturating_sub(used);
    let (system_prompt_out, trimmed_system_chars) = if system_tokens <= system_budget {
        (system_prompt.to_string(), 0)
    } else if system_budget <= est(LOCAL_CTX_TRIM_MARKER) {
        (String::new(), system_prompt.chars().count())
    } else {
        // Keep the head: the identity / rules sections come first in every
        // DuDuClaw system prompt, the long tail is memory and skill text.
        let ratio = (system_budget - est(LOCAL_CTX_TRIM_MARKER)) as f64 / system_tokens as f64;
        let total_chars = system_prompt.chars().count();
        let keep_chars = ((total_chars as f64) * ratio).floor() as usize;
        let head: String = system_prompt.chars().take(keep_chars).collect();
        (format!("{}{}", head.trim_end(), LOCAL_CTX_TRIM_MARKER), total_chars - keep_chars)
    };
    FittedRequest {
        system_prompt: system_prompt_out,
        tools: kept,
        trimmed_tools,
        trimmed_system_chars,
    }
}

/// Ask a llama.cpp-style server for its context window (`GET /props` →
/// `default_generation_settings.n_ctx`). `None` for engines/servers that do
/// not expose one — the request is then sent untrimmed, as before.
async fn probe_context_window(provider: &LocalChatProvider) -> Option<u64> {
    let base = provider.compat_base_url()?;
    let root = base.trim_end_matches('/').trim_end_matches("/v1").to_string();
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(3))
        .build()
        .ok()?;
    let v: serde_json::Value = client.get(format!("{root}/props")).send().await.ok()?.json().await.ok()?;
    let n_ctx = v
        .get("default_generation_settings")
        .and_then(|d| d.get("n_ctx"))
        .and_then(|n| n.as_u64())
        .filter(|n| *n >= 512)?;
    Some(n_ctx)
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

    // ── Fit the request to the served model's context window ──
    // A local model is small in every dimension the cloud path never has
    // to think about: the DuDuClaw OS appliance serves 8192 tokens by
    // default and a full agent system prompt plus the whole MCP tool
    // registry came to ~33 k tokens on the first live run (llama-server
    // answered `HTTP 400 … exceeds the available context size`). llama.cpp
    // publishes the window on `/props`; when it is known, trim tools and
    // system prompt to fit instead of failing every round.
    let context_window = probe_context_window(&provider).await;
    let (system_prompt, tools) = match context_window {
        Some(n_ctx) => {
            let fit = fit_request_to_context(system_prompt, prompt, tools, n_ctx);
            if fit.trimmed_tools > 0 || fit.trimmed_system_chars > 0 {
                warn!(
                    agent = %agent_id,
                    n_ctx,
                    kept_tools = fit.tools.len(),
                    dropped_tools = fit.trimmed_tools,
                    dropped_system_chars = fit.trimmed_system_chars,
                    "local tool loop: request trimmed to the served model's context window"
                );
            }
            (fit.system_prompt, fit.tools)
        }
        None => (system_prompt.to_string(), tools),
    };
    let system_prompt = system_prompt.as_str();

    let model = model_id
        .filter(|m| !m.trim().is_empty())
        .unwrap_or_else(|| provider.model())
        .to_string();
    // Cloned because `model` is still needed after `req` is moved into the
    // tool loop (WP-6E probe attribution).
    let mut req = ChatRequest::new(model.clone());
    let system = system_prompt.trim();
    if !system.is_empty() {
        // Local servers (vLLM APC / SGLang RadixAttention) prefix-cache
        // implicitly; no explicit breakpoint needed.
        req.system.push(SystemBlock::uncached(system));
    }
    req.messages.push(ChatMessage::user(prompt));
    req.tools = tools;

    // P1-4: enforce the agent's static PolicyKernel policy on this local
    // tool-loop path (complete mediation, I3). Empty policy → the kernel
    // abstains (passthrough). Wrapping the registry keeps `run_tool_loop`
    // untouched.
    let empty_policy: Vec<duduclaw_core::types::ToolPolicy> = Vec::new();
    let policy = capabilities.map(|c| c.policy.as_slice()).unwrap_or(&empty_policy);
    let guarded = duduclaw_llm::PolicyExecutor::new(&registry, policy, agent_id);

    // WP-6E: Code Mode Phase 0 measurement gate
    // (`commercial/docs/DESIGN-code-mode-2026-08.md` §8.1) — beneficiary #3 of
    // the design's §2 list. Pure observation; forwards requests/responses
    // verbatim and only counts.
    let probe = crate::tool_loop_probe::ToolLoopProbe::new(&provider);
    let loop_result = duduclaw_llm::run_tool_loop(
        &probe,
        req,
        &guarded,
        duduclaw_llm::DEFAULT_MAX_TOOL_ITERS,
    )
    .await;
    probe.finish_and_record(
        agent_id,
        crate::tool_loop_probe::ProbePath::LocalInference,
        &model,
    );
    match loop_result {
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
    use super::{fit_request_to_context, LOCAL_CTX_RESERVE_TOKENS, LOCAL_CTX_TRIM_MARKER};
    use duduclaw_llm::ToolDef;

    fn tool(name: &str, desc_len: usize) -> ToolDef {
        ToolDef {
            name: name.to_string(),
            description: "x".repeat(desc_len),
            input_schema: serde_json::json!({"type": "object", "properties": {}}),
        }
    }

    #[test]
    fn fit_is_a_no_op_when_everything_already_fits() {
        let tools = vec![tool("tasks_claim", 40), tool("read_file", 40)];
        let fit = fit_request_to_context("be helpful", "hi", tools, 8192);
        assert_eq!(fit.tools.len(), 2);
        assert_eq!(fit.trimmed_tools, 0);
        assert_eq!(fit.trimmed_system_chars, 0);
        assert_eq!(fit.system_prompt, "be helpful");
    }

    #[test]
    fn fit_drops_tools_in_order_but_always_keeps_tasks_tools() {
        // ~1000 tokens per tool × 20 tools ≫ an 8192 window.
        let mut tools: Vec<ToolDef> = (0..18).map(|i| tool(&format!("tool_{i:02}"), 4000)).collect();
        tools.push(tool("tasks_claim", 4000));
        tools.push(tool("tasks_complete", 4000));
        let fit = fit_request_to_context("sys", "prompt", tools, 8192);
        assert!(fit.trimmed_tools > 0);
        let names: Vec<&str> = fit.tools.iter().map(|t| t.name.as_str()).collect();
        assert!(names.contains(&"tasks_claim"), "{names:?}");
        assert!(names.contains(&"tasks_complete"), "{names:?}");
        // Earlier tools are preferred over later ones.
        let first_dropped = (0..18).find(|i| !names.contains(&format!("tool_{i:02}").as_str())).unwrap();
        assert!((first_dropped..18).all(|i| !names.contains(&format!("tool_{i:02}").as_str())), "{names:?}");
    }

    #[test]
    fn fit_truncates_the_system_prompt_with_a_visible_marker() {
        // 40 000 ASCII chars ≈ 10 000 tokens against a 4096 window.
        let system = "a".repeat(40_000);
        let fit = fit_request_to_context(&system, "prompt", vec![tool("tasks_claim", 40)], 4096);
        assert!(fit.trimmed_system_chars > 0);
        assert!(fit.system_prompt.ends_with(LOCAL_CTX_TRIM_MARKER));
        let kept = super::super::prompt_compression::estimate_tokens(&fit.system_prompt);
        assert!(kept + LOCAL_CTX_RESERVE_TOKENS < 4096, "kept {kept} tokens");
    }

    #[test]
    fn fit_never_leaves_the_prompt_itself_without_room() {
        // The user prompt alone eats most of the window: tools and system go.
        let prompt = "p".repeat(20_000); // ≈ 5000 tokens
        let fit = fit_request_to_context("system text here", &prompt, vec![tool("read_file", 400)], 6000);
        assert_eq!(fit.tools.len(), 0);
        assert!(fit.system_prompt.is_empty() || fit.system_prompt.ends_with(LOCAL_CTX_TRIM_MARKER));
    }

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
