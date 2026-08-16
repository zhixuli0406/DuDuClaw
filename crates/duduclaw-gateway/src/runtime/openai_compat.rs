//! OpenAI-compatible API runtime — works with MiniMax, DeepSeek, OpenRouter, etc.
//!
//! Uses the standard `/v1/chat/completions` endpoint with SSE streaming.
//! MiniMax-specific notes:
//!   - base_url: https://api.minimax.io/v1
//!   - Ignores `presence_penalty`, `frequency_penalty`, `logit_bias`
//!   - Supports MiniMax-M2.7, MiniMax-M2.5, etc.

use async_trait::async_trait;
use duduclaw_core::truncate_bytes;
use futures_util::stream::BoxStream;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use duduclaw_llm::providers::OpenAiCompatProvider;
// NOTE: `duduclaw_llm::ChatMessage` is referenced fully-qualified below — the
// local `ChatMessage` (this module's chat/completions wire struct) shadows the
// name, so it is deliberately NOT imported here.
use duduclaw_llm::{
    ApiAuth, ChatProvider, ChatRequest, ChatResponse, ContentPart, LlmError, NormalizedUsage, Role,
    StreamEvent, SystemBlock,
};

use super::{AgentRuntime, RuntimeContext, RuntimeResponse};

/// Runtime that calls any OpenAI-compatible chat completions API.
pub struct OpenAiCompatRuntime;

impl OpenAiCompatRuntime {
    pub fn new() -> Self {
        Self
    }
}

// ── API types ───────────────────────────────────────────────────

#[derive(Debug, Serialize)]
struct ChatCompletionRequest {
    model: String,
    messages: Vec<ChatMessage>,
    max_tokens: u32,
    stream: bool,
}

#[derive(Debug, Serialize)]
struct ChatMessage {
    role: String,
    content: String,
}

#[derive(Debug, Deserialize)]
struct ChatCompletionResponse {
    choices: Vec<Choice>,
    usage: Option<CompletionUsage>,
}

#[derive(Debug, Deserialize)]
struct Choice {
    message: Option<ChoiceMessage>,
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ChoiceMessage {
    content: Option<String>,
    /// Reasoning-model providers (xAI Grok, DeepSeek R1) put chain-of-thought
    /// here; when `finish_reason = "length"` hits mid-reasoning, `content` can
    /// be empty while this field is populated. Only used for diagnostics —
    /// reasoning text is never sent to the user as the reply.
    reasoning_content: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CompletionUsage {
    #[serde(default)]
    prompt_tokens: u64,
    #[serde(default)]
    completion_tokens: u64,
}

#[derive(Debug, Deserialize)]
struct ApiErrorResponse {
    error: Option<ApiErrorDetail>,
}

#[derive(Debug, Deserialize)]
struct ApiErrorDetail {
    message: String,
}

// ── Shared HTTP client ──────────────────────────────────────────

static HTTP_CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();

fn http_client() -> &'static reqwest::Client {
    HTTP_CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(120))
            .connect_timeout(std::time::Duration::from_secs(10))
            .build()
            .expect("Failed to build HTTP client")
    })
}

// ── AgentRuntime impl ───────────────────────────────────────────

#[async_trait]
impl AgentRuntime for OpenAiCompatRuntime {
    fn name(&self) -> &str {
        "openai_compat"
    }

    async fn execute(
        &self,
        prompt: &str,
        context: &RuntimeContext,
    ) -> Result<RuntimeResponse, String> {
        // Look up provider config from config.toml accounts, honouring agent's preferred_provider
        let (api_key, base_url, provider_name) = resolve_provider_config(
            &context.home_dir,
            &context.agent_id,
            context.preferred_provider.as_deref(),
        )
        .await?;

        // MCP tool surface (root-cause fix): when the agent has reachable MCP
        // tools, drive the model through duduclaw-llm's `run_tool_loop` so
        // API-mode non-Claude models (Grok / DeepSeek / MiniMax / …) can
        // actually CALL tools (Odoo, memory, channel, …) instead of narrating
        // "I'll go look that up" and stopping. `Ok(None)` ⇒ no reachable tools
        // (MCP child failed to spawn, or the capability filter left none) so we
        // degrade to the plain-messages path below. A provider/model error
        // during the loop is propagated (never silently re-tried on the plain
        // path — the same call would fail again; failover handles it upstream).
        match self
            .execute_with_tools(prompt, context, &api_key, &base_url, &provider_name)
            .await
        {
            Ok(Some(response)) => return Ok(response),
            Ok(None) => {
                info!(
                    agent = %context.agent_id,
                    "OpenAiCompatRuntime: no reachable MCP tools — using plain messages path"
                );
            }
            Err(e) => return Err(e),
        }

        self.execute_plain(prompt, context, &api_key, &base_url)
            .await
    }

    async fn is_available(&self) -> bool {
        // Check if any OpenAI-compatible provider API key is configured.
        // WP-8B (credentials doctrine P3): derived from the canonical
        // `duduclaw_core::provider_env` table for every provider this
        // runtime's `PROVIDERS` preset actually knows how to reach, instead
        // of a hand-maintained literal list (which had drifted — it used to
        // include TOGETHER_API_KEY / MISTRAL_API_KEY even though neither
        // "together" nor "mistral" has a `PROVIDERS` preset, so setting only
        // one of those two env vars made this return `true` while
        // `resolve_provider_config`'s env-var steps could never actually use
        // it — a config.toml `[[accounts]]` entry is the only way those two
        // providers resolve here, and this env-only probe can't see that
        // either way).
        PROVIDERS.iter().any(|p| {
            duduclaw_core::provider_env::provider_env_key_names(p.name)
                .iter()
                .any(|k| std::env::var(k).is_ok_and(|v| !v.is_empty()))
        })
    }
}

impl OpenAiCompatRuntime {
    /// The plain, tools-less chat/completions path (pre-tool-loop behavior).
    ///
    /// Preserves the empty-content⇒Err classification and the historical
    /// empty-turn filter from the reasoning-model hardening batch. Credentials
    /// are resolved by the caller so the tool path and this path agree on the
    /// account without a second config read.
    async fn execute_plain(
        &self,
        prompt: &str,
        context: &RuntimeContext,
        api_key: &str,
        base_url: &str,
    ) -> Result<RuntimeResponse, String> {
        info!(
            agent = %context.agent_id,
            model = %context.model,
            base_url = %base_url,
            "OpenAiCompatRuntime: calling chat/completions"
        );

        let client = http_client();

        // System prompt, then prior turns (RFC-25 A1 — native multi-turn so the
        // agent keeps context across turns), then the current user message.
        let mut messages = Vec::with_capacity(context.conversation_history.len() + 2);
        messages.push(ChatMessage {
            role: "system".to_string(),
            content: context.system_prompt.clone(),
        });
        for turn in &context.conversation_history {
            // Skip empty turns: strict OpenAI-compat providers reject them, and
            // an empty assistant turn (from a past dropped reply) teaches the
            // model to answer with nothing — self-reinforcing session breakage.
            if turn.content.trim().is_empty() {
                continue;
            }
            messages.push(ChatMessage {
                role: turn.role.clone(),
                content: turn.content.clone(),
            });
        }
        messages.push(ChatMessage {
            role: "user".to_string(),
            content: prompt.to_string(),
        });

        let body = ChatCompletionRequest {
            model: context.model.clone(),
            messages,
            max_tokens: context.max_tokens,
            stream: false,
        };

        let url = format!("{}/chat/completions", base_url.trim_end_matches('/'));
        let response = client
            .post(&url)
            .header("Authorization", format!("Bearer {api_key}"))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("HTTP request to {url} failed: {e}"))?;

        let status = response.status();
        let response_text = response
            .text()
            .await
            .map_err(|e| format!("Failed to read response body: {e}"))?;

        if !status.is_success() {
            if let Ok(err) = serde_json::from_str::<ApiErrorResponse>(&response_text) {
                if let Some(detail) = err.error {
                    return Err(format!("API error ({status}): {}", detail.message));
                }
            }
            return Err(format!(
                "API error ({status}): {}",
                truncate_bytes(&response_text, 300)
            ));
        }

        let parsed: ChatCompletionResponse = serde_json::from_str(&response_text)
            .map_err(|e| format!("Failed to parse response: {e}"))?;

        let first_choice = parsed.choices.first();
        let content = first_choice
            .and_then(|c| c.message.as_ref())
            .and_then(|m| m.content.clone())
            .unwrap_or_default();

        // Empty content is an ERROR, never a success. Reasoning models (Grok,
        // DeepSeek R1) can burn the whole `max_tokens` budget on
        // `reasoning_content` and finish with `finish_reason = "length"` and no
        // visible text; returning Ok("") here would be silently dropped by every
        // channel and poison the session with an empty assistant turn.
        if content.trim().is_empty() {
            let finish = first_choice
                .and_then(|c| c.finish_reason.as_deref())
                .unwrap_or("unknown");
            let reasoning_len = first_choice
                .and_then(|c| c.message.as_ref())
                .and_then(|m| m.reasoning_content.as_deref())
                .map(|r| r.len())
                .unwrap_or(0);
            return Err(format!(
                "Empty response from OpenAI-compat API (model={} finish_reason={finish} \
                 reasoning_content_bytes={reasoning_len}); \
                 if finish_reason=length, the model spent the whole max_tokens budget on \
                 reasoning before emitting any answer text",
                context.model
            ));
        }

        let (input_tokens, output_tokens) = parsed
            .usage
            .map(|u| (u.prompt_tokens, u.completion_tokens))
            .unwrap_or((0, 0));

        Ok(RuntimeResponse {
            content,
            input_tokens,
            output_tokens,
            cache_read_tokens: 0,
            model_used: context.model.clone(),
            runtime_name: "openai_compat".to_string(),
        })
    }

    /// Drive the model through the MCP tool loop over the openai-compat
    /// provider, giving API-mode non-Claude models the full duduclaw MCP tool
    /// surface (the root-cause fix — previously these models only ever saw
    /// plain messages and could never call a tool).
    ///
    /// Returns:
    /// - `Ok(Some(resp))` — the loop produced a non-empty answer.
    /// - `Ok(None)` — no reachable tools (MCP child failed to spawn, or the
    ///   capability filter removed every tool). The caller degrades to the
    ///   plain-messages path. This is the fail-soft branch: having no tools is
    ///   never worse than the pre-fix behavior.
    /// - `Err(e)` — a provider/model transport error, or the loop finished with
    ///   no answer text (EmptyResponse). Propagated so `failover.rs` classifies
    ///   it, exactly like the plain path's empty-content⇒Err.
    ///
    /// Task-scoped grants (`[capabilities] scoped_tools` / PORTICO) and
    /// `approval_required_tools` are enforced on the mcp-server side at the MCP
    /// dispatch gate — the mandatory mediation point — so they are NOT
    /// re-implemented here; this path only pre-filters the advertised tool
    /// surface and applies the static PolicyKernel policy (parity with the
    /// direct-API / local-inference tool loops).
    async fn execute_with_tools(
        &self,
        prompt: &str,
        context: &RuntimeContext,
        api_key: &str,
        base_url: &str,
        provider_name: &str,
    ) -> Result<Option<RuntimeResponse>, String> {
        // Phase A — MCP tool registry (fail-safe: any spawn/handshake/list
        // failure logs a warn and yields None ⇒ plain-messages degrade).
        let Some(registry) = crate::claude_runner::build_mcp_tool_registry(&context.agent_id).await
        else {
            warn!(
                agent = %context.agent_id,
                "OpenAiCompatRuntime: MCP registry unavailable — degrading to plain messages"
            );
            return Ok(None);
        };

        // Fail-closed capability filter — a denied tool is never advertised, and
        // a non-empty `allowed_tools` allowlist keeps only its intersection.
        let tools = crate::claude_runner::filter_tool_defs(
            registry.tool_defs(),
            context.capabilities.as_ref(),
        );
        if tools.is_empty() {
            info!(
                agent = %context.agent_id,
                "OpenAiCompatRuntime: capability filter left no tools — plain messages path"
            );
            return Ok(None);
        }

        // Phase B — provider + loop. A provider/model error here propagates.
        let auth = ApiAuth::new(api_key.to_string()).with_base_url(base_url.to_string());
        let base_provider =
            OpenAiCompatProvider::new(provider_name.to_string(), auth, base_url.to_string());
        // Tap accumulates usage across every loop iteration into one billable
        // total (the loop re-sends the prefix each round — summing per-call
        // usage is the true billed cost fed back into RuntimeResponse).
        let provider = UsageTap::new(base_provider);

        let model_id = compose_model_id(provider_name, &context.model);
        let mut req = build_tool_chat_request(context, prompt, model_id);
        req.tools = tools;

        // PolicyKernel static policy (complete mediation, I3). Empty policy ⇒
        // the kernel abstains (passthrough) — byte-identical to no policy.
        let empty_policy: Vec<duduclaw_core::types::ToolPolicy> = Vec::new();
        let policy = context
            .capabilities
            .as_ref()
            .map(|c| c.policy.as_slice())
            .unwrap_or(&empty_policy);
        let guarded =
            duduclaw_llm::PolicyExecutor::new(&registry, policy, context.agent_id.as_str());

        info!(
            agent = %context.agent_id,
            model = %context.model,
            base_url = %base_url,
            tools = req.tools.len(),
            "OpenAiCompatRuntime: MCP tool loop"
        );

        // WP-A5 (design commercial/docs/design-task-forward-model-2026-08-06.md
        // §5.3/§9): use the provenance-tracking variant (policy `Off` by
        // default here, so behavior is otherwise byte-identical to
        // `run_tool_loop`) purely to get `ToolLoopOutcome.tool_calls` — the
        // A3 forward-model's smallest `Full`-fidelity upgrade path. The
        // collected calls are pushed into the SAME `NATIVE_TOOL_COLLECTOR`
        // task-local that claude_runner.rs (WP-A4) and codex/gemini (T10)
        // use, so `dispatcher.rs`'s bridging logic doesn't need a
        // runtime-specific branch.
        // WP-6E: Code Mode Phase 0 measurement gate
        // (`commercial/docs/DESIGN-code-mode-2026-08.md` §8.1). A pure
        // observation decorator layered OVER the billing tap — it forwards
        // every request/response verbatim and only counts rounds, tool-schema
        // tokens and provider-reported cache reads. This path is beneficiary
        // #1 of the design's §2 list, so it is the primary measurement site.
        let probe = crate::tool_loop_probe::ToolLoopProbe::new(&provider);
        let loop_result = duduclaw_llm::run_tool_loop_with_provenance(
            &probe,
            req,
            &guarded,
            duduclaw_llm::DEFAULT_MAX_TOOL_ITERS,
            duduclaw_llm::ProvenanceConfig::default(),
        )
        .await;
        // Recorded BEFORE the `?`: a turn that died mid-loop still measured
        // real provider rounds, and dropping it would bias the gate toward
        // short, cheap turns.
        probe.finish_and_record(
            &context.agent_id,
            crate::tool_loop_probe::ProbePath::OpenAiCompat,
            &context.model,
        );
        let loop_outcome =
            loop_result.map_err(|e| format!("openai-compat tool loop error: {e}"))?;
        // R1: `LoopToolCall` already carries masked+capped result/input text
        // (masking happens inside `duduclaw-llm::tool_loop`, which already
        // depends on `duduclaw-security` for `PolicyExecutor`) — carried
        // through verbatim, never re-masked here.
        crate::runtime::extend_native_tool_events(
            loop_outcome
                .tool_calls
                .into_iter()
                .map(|call| crate::runtime::NativeToolEvent {
                    tool_name: call.tool_name,
                    success: call.success,
                    result_text: call.result_text,
                    input_text: call.input_text,
                })
                .collect(),
        );
        let resp = loop_outcome.response;

        // A tool-only round is NOT an empty reply — the loop keeps going. Only a
        // loop that terminates with no answer text is an EmptyResponse.
        let content = classify_final_text(&resp, &context.model)?;
        let usage = provider.total();

        Ok(Some(RuntimeResponse {
            content,
            input_tokens: usage.input_tokens,
            // reasoning tokens are billed at the output rate; compat providers
            // fold them into output_tokens (reasoning_tokens = 0) but sum both
            // for cross-provider correctness.
            output_tokens: usage.output_tokens + usage.reasoning_tokens,
            cache_read_tokens: usage.cache_read_tokens,
            model_used: context.model.clone(),
            runtime_name: "openai_compat".to_string(),
        }))
    }
}

// ── Tool-loop wiring (MCP tool surface for API-mode models) ─────────────────

/// Compose a provider-prefixed model id so `duduclaw-llm`'s
/// `split_model_id` strips the provider segment and leaves the real model id
/// intact — even OpenRouter-style ids that themselves contain a `/`
/// (`openrouter/anthropic/claude-sonnet-5` → wire model `anthropic/claude-sonnet-5`).
fn compose_model_id(provider_name: &str, model: &str) -> String {
    if model.is_empty() {
        provider_name.to_string()
    } else {
        format!("{provider_name}/{model}")
    }
}

/// Build the normalized [`ChatRequest`] for the tool loop from a
/// [`RuntimeContext`]: system prompt as one uncached block (compat providers
/// prefix-cache implicitly), prior turns (preserving the empty-turn filter),
/// then the current user message.
fn build_tool_chat_request(
    context: &RuntimeContext,
    prompt: &str,
    model_id: String,
) -> ChatRequest {
    let mut req = ChatRequest::new(model_id);
    if !context.system_prompt.trim().is_empty() {
        req.system
            .push(SystemBlock::uncached(context.system_prompt.clone()));
    }
    for turn in &context.conversation_history {
        // Same empty-turn filter as the plain path: an empty assistant turn
        // teaches the model to answer with nothing (session breakage).
        if turn.content.trim().is_empty() {
            continue;
        }
        let role = if turn.role == "assistant" {
            Role::Assistant
        } else {
            Role::User
        };
        req.messages.push(duduclaw_llm::ChatMessage {
            role,
            parts: vec![ContentPart::Text(turn.content.clone())],
        });
    }
    req.messages
        .push(duduclaw_llm::ChatMessage::user(prompt.to_string()));
    req.max_tokens = context.max_tokens;
    req
}

/// Extract the final answer text, treating an empty final answer as an error
/// (mirrors the plain path's empty-content⇒Err). A tool-only intermediate
/// round never reaches here — the loop only returns on a non-`ToolUse` stop or
/// iteration-cap exhaustion.
fn classify_final_text(resp: &ChatResponse, model: &str) -> Result<String, String> {
    let text = resp.text();
    if text.trim().is_empty() {
        return Err(format!(
            "Empty response from OpenAI-compat tool loop (model={model} stop={:?}); \
             the model finished without emitting any answer text after tool use",
            resp.stop
        ));
    }
    Ok(text)
}

/// A [`ChatProvider`] decorator that accumulates [`NormalizedUsage`] across
/// every `complete()` call the tool loop makes, so the multi-round token spend
/// lands in one `RuntimeResponse`. Generic over the inner provider so the
/// accumulation is unit-testable with a scripted mock (no HTTP).
struct UsageTap<P> {
    inner: P,
    total: std::sync::Mutex<NormalizedUsage>,
}

impl<P> UsageTap<P> {
    fn new(inner: P) -> Self {
        Self {
            inner,
            total: std::sync::Mutex::new(NormalizedUsage::default()),
        }
    }

    fn total(&self) -> NormalizedUsage {
        self.total.lock().map(|g| *g).unwrap_or_default()
    }
}

#[async_trait]
impl<P: ChatProvider> ChatProvider for UsageTap<P> {
    fn id(&self) -> &str {
        self.inner.id()
    }

    async fn complete(&self, req: &ChatRequest) -> Result<ChatResponse, LlmError> {
        let resp = self.inner.complete(req).await?;
        if let Ok(mut t) = self.total.lock() {
            *t = t.saturating_add(&resp.usage);
        }
        Ok(resp)
    }

    async fn stream(
        &self,
        req: &ChatRequest,
    ) -> Result<BoxStream<'static, Result<StreamEvent, LlmError>>, LlmError> {
        // Unused by the tool loop (non-streaming); delegate for completeness.
        self.inner.stream(req).await
    }
}

// ── Provider config resolution ──────────────────────────────────

/// Known provider presets.
pub struct ProviderPreset {
    pub name: &'static str,
    pub base_url: &'static str,
    pub default_model: &'static str,
}

/// Built-in provider presets.
pub const PROVIDERS: &[ProviderPreset] = &[
    ProviderPreset {
        name: "minimax",
        base_url: "https://api.minimax.io/v1",
        default_model: "MiniMax-M2.7",
    },
    ProviderPreset {
        name: "deepseek",
        base_url: "https://api.deepseek.com/v1",
        default_model: "deepseek-chat",
    },
    ProviderPreset {
        name: "openrouter",
        base_url: "https://openrouter.ai/api/v1",
        default_model: "anthropic/claude-sonnet-4",
    },
    ProviderPreset {
        name: "groq",
        base_url: "https://api.groq.com/openai/v1",
        default_model: "llama-3.3-70b-versatile",
    },
    ProviderPreset {
        name: "openai",
        base_url: "https://api.openai.com/v1",
        default_model: "gpt-4o",
    },
    ProviderPreset {
        name: "xai",
        base_url: "https://api.x.ai/v1",
        default_model: "grok-4.1-fast",
    },
];

/// Resolve API key, base URL, and provider name for an OpenAI-compatible
/// provider.
///
/// The provider name (third tuple element) is what the tool-loop path uses to
/// compose a provider-prefixed model id (see [`compose_model_id`]) so
/// `split_model_id` never mangles the real model, and to label the provider on
/// the normalized request/response.
///
/// Lookup order:
/// 1. If `preferred_provider` is set, try that provider's env var first.
/// 2. Environment variable: `{PROVIDER}_API_KEY` (e.g., MINIMAX_API_KEY, DEEPSEEK_API_KEY)
/// 3. Generic `OPENAI_API_KEY` with provider-specific base_url
/// 4. Config file accounts with `provider = "..."` and `base_url = "..."`
async fn resolve_provider_config(
    home_dir: &std::path::Path,
    _agent_id: &str,
    preferred_provider: Option<&str>,
) -> Result<(String, String, String), String> {
    // 1. If agent specifies a provider, try that first
    if let Some(provider_name) = preferred_provider {
        if let Some(provider) = PROVIDERS.iter().find(|p| p.name == provider_name) {
            if let Some(key) = duduclaw_core::provider_env::resolve_env_key(provider.name) {
                return Ok((
                    key,
                    provider.base_url.to_string(),
                    provider.name.to_string(),
                ));
            }
        }
    }

    // 2. Check provider-specific env vars (WP-8B: canonical
    // `duduclaw_core::provider_env` table instead of a re-derived
    // `{PROVIDER}_API_KEY` string — same names for every provider in this
    // preset list today, but now shared with the rest of the workspace
    // instead of independently derived here).
    for provider in PROVIDERS {
        if let Some(key) = duduclaw_core::provider_env::resolve_env_key(provider.name) {
            return Ok((
                key,
                provider.base_url.to_string(),
                provider.name.to_string(),
            ));
        }
    }

    // Fallback to OPENAI_API_KEY with default OpenAI base
    if let Ok(key) = std::env::var("OPENAI_API_KEY") {
        if !key.is_empty() {
            return Ok((
                key,
                "https://api.openai.com/v1".to_string(),
                "openai".to_string(),
            ));
        }
    }

    // Try reading from config.toml
    let config_path = home_dir.join("config.toml");
    if let Ok(content) = tokio::fs::read_to_string(&config_path).await {
        if let Ok(table) = content.parse::<toml::Table>() {
            // WP-6C follow-up (#1): this used to be an 8th hand-rolled
            // decrypt-or-plaintext dialect with no `secret://` awareness at
            // all — an `api_key = "secret://vault/…"` reference was returned
            // verbatim and sent to the vendor **as the bearer token** (the
            // same class of bug WP-H1/WP-6C fixed everywhere else). Converged
            // onto the single `SecretRef` resolver: `[secret_manager]` is
            // read from the same already-parsed table (no extra disk read),
            // and this function is `async fn` on every call path (both
            // `execute()` and `execute_sse_streaming()` already `.await` it),
            // so a network-backed reference (Vault / 1Password / Infisical)
            // resolves for real instead of failing closed.
            let sm_cfg: duduclaw_security::secret_manager::SecretManagerConfig = table
                .get("secret_manager")
                .cloned()
                .and_then(|v| v.try_into().ok())
                .unwrap_or_default();
            if let Some(accounts) = table.get("accounts").and_then(|a| a.as_array()) {
                for acc in accounts {
                    let provider = acc.get("provider").and_then(|p| p.as_str()).unwrap_or("");
                    let base_url = acc.get("base_url").and_then(|u| u.as_str());

                    if !provider.is_empty() {
                        let secret_ref = duduclaw_security::secret_ref::SecretRef::from_typed(
                            acc.get("api_key_enc").and_then(|v| v.as_str()),
                            acc.get("api_key").and_then(|v| v.as_str()).unwrap_or(""),
                        );
                        // Same warning as before, gated on the same condition
                        // (a bare plaintext key, not `_enc` and not a
                        // `secret://` reference) instead of firing on every
                        // encrypted-field-decrypt-failure fallback.
                        if secret_ref.describe().source == duduclaw_security::secret_ref::SourceKind::Legacy {
                            tracing::warn!(
                                provider,
                                "OpenAI-compat account uses plaintext api_key; \
                                 migrate to api_key_enc for better security"
                            );
                        }
                        let api_key_opt = secret_ref
                            .resolve(&sm_cfg, home_dir)
                            .await
                            .map(|s| s.expose_owned());

                        if let Some(key) = api_key_opt {
                            let url = base_url
                                .map(|u| u.to_string())
                                .or_else(|| {
                                    PROVIDERS
                                        .iter()
                                        .find(|p| p.name == provider)
                                        .map(|p| p.base_url.to_string())
                                })
                                .unwrap_or_else(|| "https://api.openai.com/v1".to_string());
                            if !url.starts_with("https://") && !url.starts_with("http://") {
                                return Err(format!(
                                    "Invalid base_url scheme: {url}. Must use http:// or https://"
                                ));
                            }
                            return Ok((key, url, provider.to_string()));
                        }
                    }
                }
            }
        }
    }

    Err("No OpenAI-compatible API key found. Set MINIMAX_API_KEY, DEEPSEEK_API_KEY, or OPENAI_API_KEY".to_string())
}

// ── SSE streaming ───────────────────────────────────────────────

impl OpenAiCompatRuntime {
    /// Execute with SSE streaming — parses `data: {"choices":[{"delta":{"content":"..."}}]}` events.
    pub async fn execute_sse_streaming(
        &self,
        prompt: &str,
        context: &super::RuntimeContext,
    ) -> Result<super::RuntimeResponse, String> {
        let (api_key, base_url, _provider_name) = resolve_provider_config(
            &context.home_dir,
            &context.agent_id,
            context.preferred_provider.as_deref(),
        )
        .await?;
        let client = http_client();
        let messages = vec![
            ChatMessage {
                role: "system".to_string(),
                content: context.system_prompt.clone(),
            },
            ChatMessage {
                role: "user".to_string(),
                content: prompt.to_string(),
            },
        ];
        let body = ChatCompletionRequest {
            model: context.model.clone(),
            messages,
            max_tokens: context.max_tokens,
            stream: true,
        };
        let url = format!("{}/chat/completions", base_url.trim_end_matches('/'));
        let response = client
            .post(&url)
            .header("Authorization", format!("Bearer {api_key}"))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("SSE request failed: {e}"))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(format!(
                "SSE error ({status}): {}",
                body.chars().take(300).collect::<String>()
            ));
        }

        // Stream SSE chunks instead of buffering the entire response
        let mut stream = response.bytes_stream();
        let mut buf: Vec<u8> = Vec::new();
        let mut content = String::new();
        let mut input_tokens: u64 = 0;
        let mut output_tokens: u64 = 0;
        let mut done = false;

        while let Some(chunk) = stream.next().await {
            if done {
                break;
            }
            let chunk = chunk.map_err(|e| format!("SSE stream error: {e}"))?;
            buf.extend_from_slice(&chunk);

            // Process all complete lines available in the buffer
            while let Some(pos) = buf.iter().position(|&b| b == b'\n') {
                let line_bytes = buf.drain(..pos + 1).collect::<Vec<u8>>();
                let line = String::from_utf8_lossy(&line_bytes);
                let line = line.trim();

                if line.is_empty() || line.starts_with(':') {
                    continue;
                }

                if let Some(json_str) = line.strip_prefix("data: ") {
                    if json_str == "[DONE]" {
                        done = true;
                        break;
                    }
                    if let Ok(chunk) = serde_json::from_str::<serde_json::Value>(json_str) {
                        if let Some(delta) = chunk
                            .pointer("/choices/0/delta/content")
                            .and_then(|v| v.as_str())
                        {
                            content.push_str(delta);
                        }
                        if let Some(usage) = chunk.get("usage") {
                            input_tokens = usage
                                .get("prompt_tokens")
                                .and_then(|v| v.as_u64())
                                .unwrap_or(input_tokens);
                            output_tokens = usage
                                .get("completion_tokens")
                                .and_then(|v| v.as_u64())
                                .unwrap_or(output_tokens);
                        }
                    }
                }
            }
        }

        // Process any remaining data in the buffer (line without trailing newline)
        if !buf.is_empty() {
            let line = String::from_utf8_lossy(&buf);
            let line = line.trim();
            if let Some(json_str) = line.strip_prefix("data: ") {
                if json_str != "[DONE]" {
                    if let Ok(chunk) = serde_json::from_str::<serde_json::Value>(json_str) {
                        if let Some(delta) = chunk
                            .pointer("/choices/0/delta/content")
                            .and_then(|v| v.as_str())
                        {
                            content.push_str(delta);
                        }
                        if let Some(usage) = chunk.get("usage") {
                            input_tokens = usage
                                .get("prompt_tokens")
                                .and_then(|v| v.as_u64())
                                .unwrap_or(input_tokens);
                            output_tokens = usage
                                .get("completion_tokens")
                                .and_then(|v| v.as_u64())
                                .unwrap_or(output_tokens);
                        }
                    }
                }
            }
        }

        Ok(super::RuntimeResponse {
            content,
            input_tokens,
            output_tokens,
            cache_read_tokens: 0,
            model_used: context.model.clone(),
            runtime_name: "openai_compat_sse".to_string(),
        })
    }
}

// ── Tests ───────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_provider_presets() {
        assert_eq!(PROVIDERS.len(), 6);
        let minimax = PROVIDERS.iter().find(|p| p.name == "minimax").unwrap();
        assert_eq!(minimax.base_url, "https://api.minimax.io/v1");
        assert_eq!(minimax.default_model, "MiniMax-M2.7");
        // WP6: xAI (Grok) direct-API preset.
        let xai = PROVIDERS.iter().find(|p| p.name == "xai").unwrap();
        assert_eq!(xai.base_url, "https://api.x.ai/v1");
    }

    #[test]
    fn test_parse_completion_response() {
        let json = r#"{"choices":[{"message":{"role":"assistant","content":"Hello!"}}],"usage":{"prompt_tokens":10,"completion_tokens":5}}"#;
        let resp: ChatCompletionResponse = serde_json::from_str(json).unwrap();
        assert_eq!(
            resp.choices[0]
                .message
                .as_ref()
                .unwrap()
                .content
                .as_deref()
                .unwrap(),
            "Hello!"
        );
        assert_eq!(resp.usage.unwrap().prompt_tokens, 10);
    }

    #[test]
    fn test_parse_reasoning_model_empty_content() {
        // xAI Grok / DeepSeek R1 shape: reasoning burned the whole max_tokens
        // budget — content empty, reasoning_content populated, finish "length".
        // execute() must classify this as an empty response (diagnostics need
        // finish_reason + reasoning_content to survive deserialization).
        let json = r#"{"choices":[{"message":{"role":"assistant","content":"","reasoning_content":"thinking..."},"finish_reason":"length"}],"usage":{"prompt_tokens":100,"completion_tokens":8192}}"#;
        let resp: ChatCompletionResponse = serde_json::from_str(json).unwrap();
        let choice = &resp.choices[0];
        let msg = choice.message.as_ref().unwrap();
        assert_eq!(msg.content.as_deref(), Some(""));
        assert_eq!(msg.reasoning_content.as_deref(), Some("thinking..."));
        assert_eq!(choice.finish_reason.as_deref(), Some("length"));
    }

    // ── MCP tool-loop wiring (API-mode tool surface) ────────────────────────

    use duduclaw_llm::{
        run_tool_loop, StopReason, ToolDef, ToolExecutor, ToolOutcome, DEFAULT_MAX_TOOL_ITERS,
    };
    use std::sync::Mutex;

    fn tool_ctx(system: &str, model: &str) -> RuntimeContext {
        RuntimeContext {
            agent_dir: None,
            system_prompt: system.to_string(),
            model: model.to_string(),
            max_tokens: 2048,
            home_dir: std::path::PathBuf::from("/tmp"),
            agent_id: "tester".to_string(),
            preferred_provider: None,
            conversation_history: vec![],
            capabilities: None,
            account_pool: vec![],
        }
    }

    // ── compose_model_id ────────────────────────────────────────────────────

    #[test]
    fn compose_model_id_prefixes_so_split_preserves_the_real_model() {
        // Bare model: provider-prefixed, split strips the provider only.
        assert_eq!(
            compose_model_id("deepseek", "deepseek-chat"),
            "deepseek/deepseek-chat"
        );
        assert_eq!(
            duduclaw_llm::split_model_id("deepseek/deepseek-chat"),
            (Some("deepseek"), "deepseek-chat")
        );
        // OpenRouter model that itself contains a slash survives intact.
        let id = compose_model_id("openrouter", "anthropic/claude-sonnet-5");
        assert_eq!(id, "openrouter/anthropic/claude-sonnet-5");
        assert_eq!(
            duduclaw_llm::split_model_id(&id),
            (Some("openrouter"), "anthropic/claude-sonnet-5")
        );
        // Empty model degrades to the bare provider name (no dangling slash).
        assert_eq!(compose_model_id("xai", ""), "xai");
    }

    // ── build_tool_chat_request ─────────────────────────────────────────────

    #[test]
    fn build_tool_request_filters_empty_turns_and_maps_roles() {
        let mut ctx = tool_ctx("You are helpful.", "deepseek-chat");
        ctx.conversation_history = vec![
            crate::runtime::ConversationTurn {
                role: "user".into(),
                content: "first".into(),
            },
            // Empty assistant turn must be dropped (session-breakage guard).
            crate::runtime::ConversationTurn {
                role: "assistant".into(),
                content: "   ".into(),
            },
            crate::runtime::ConversationTurn {
                role: "assistant".into(),
                content: "reply".into(),
            },
        ];
        let req = build_tool_chat_request(&ctx, "now", "deepseek/deepseek-chat".into());

        // System prompt is one uncached block.
        assert_eq!(req.system.len(), 1);
        assert_eq!(req.system[0].text, "You are helpful.");
        assert_eq!(req.max_tokens, 2048);

        // Turns: "first" (user), "reply" (assistant), then the current user
        // message — the empty assistant turn is gone.
        assert_eq!(req.messages.len(), 3);
        assert_eq!(req.messages[0].role, Role::User);
        assert!(matches!(&req.messages[0].parts[0], ContentPart::Text(t) if t == "first"));
        assert_eq!(req.messages[1].role, Role::Assistant);
        assert!(matches!(&req.messages[1].parts[0], ContentPart::Text(t) if t == "reply"));
        assert_eq!(req.messages[2].role, Role::User);
        assert!(matches!(&req.messages[2].parts[0], ContentPart::Text(t) if t == "now"));
    }

    #[test]
    fn build_tool_request_omits_blank_system_block() {
        let ctx = tool_ctx("   ", "m");
        let req = build_tool_chat_request(&ctx, "hi", "p/m".into());
        assert!(
            req.system.is_empty(),
            "blank system prompt must not add a block"
        );
        assert_eq!(req.messages.len(), 1);
    }

    // ── classify_final_text ─────────────────────────────────────────────────

    #[test]
    fn classify_final_text_ok_on_text_err_on_empty() {
        let with_text = ChatResponse {
            parts: vec![ContentPart::Text("answer".into())],
            stop: StopReason::EndTurn,
            usage: NormalizedUsage::default(),
            model_used: "m".into(),
            provider: "deepseek".into(),
        };
        assert_eq!(
            classify_final_text(&with_text, "deepseek-chat").unwrap(),
            "answer"
        );

        // A loop that ended with only a (tool_use) turn and no answer text is an
        // EmptyResponse — surfaced as Err so failover fires, not Ok("").
        let no_text = ChatResponse {
            parts: vec![],
            stop: StopReason::Other(duduclaw_llm::MAX_ITERS_STOP.into()),
            usage: NormalizedUsage::default(),
            model_used: "m".into(),
            provider: "deepseek".into(),
        };
        let err = classify_final_text(&no_text, "deepseek-chat").unwrap_err();
        assert!(err.contains("Empty response"), "got: {err}");
    }

    // ── capability filtering (deny-by-default; allow = intersection) ────────

    #[test]
    fn capability_filter_drops_denied_and_intersects_allowed() {
        use duduclaw_core::types::CapabilitiesConfig;
        let defs = || {
            vec![
                ToolDef {
                    name: "memory_search".into(),
                    description: "".into(),
                    input_schema: serde_json::json!({}),
                },
                ToolDef {
                    name: "odoo_query".into(),
                    description: "".into(),
                    input_schema: serde_json::json!({}),
                },
                ToolDef {
                    name: "channel_send".into(),
                    description: "".into(),
                    input_schema: serde_json::json!({}),
                },
            ]
        };

        // Denied tool is removed; the rest remain.
        let denied = CapabilitiesConfig {
            denied_tools: vec!["odoo_query".into()],
            ..Default::default()
        };
        let out = crate::claude_runner::filter_tool_defs(defs(), Some(&denied));
        let names: Vec<&str> = out.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, vec!["memory_search", "channel_send"]);

        // Non-empty allowlist keeps only its intersection.
        let allowed = CapabilitiesConfig {
            allowed_tools: vec!["memory_search".into(), "not_present".into()],
            ..Default::default()
        };
        let out = crate::claude_runner::filter_tool_defs(defs(), Some(&allowed));
        let names: Vec<&str> = out.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, vec!["memory_search"]);

        // deny wins over allow for the same tool.
        let both = CapabilitiesConfig {
            allowed_tools: vec!["memory_search".into(), "odoo_query".into()],
            denied_tools: vec!["odoo_query".into()],
            ..Default::default()
        };
        let out = crate::claude_runner::filter_tool_defs(defs(), Some(&both));
        let names: Vec<&str> = out.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, vec!["memory_search"]);
    }

    /// Entries written by the dashboard tool picker are Claude-CLI-qualified
    /// (`mcp__duduclaw__<name>`); the API-path registry advertises bare names.
    /// Both directions must match after namespace-prefix normalization.
    #[test]
    fn capability_filter_matches_mcp_qualified_entries() {
        use duduclaw_core::types::CapabilitiesConfig;
        let defs = || {
            vec![
                ToolDef {
                    name: "office_script".into(),
                    description: "".into(),
                    input_schema: serde_json::json!({}),
                },
                ToolDef {
                    name: "memory_search".into(),
                    description: "".into(),
                    input_schema: serde_json::json!({}),
                },
            ]
        };

        // Qualified deny drops the bare-named tool.
        let denied = CapabilitiesConfig {
            denied_tools: vec!["mcp__duduclaw__office_script".into()],
            ..Default::default()
        };
        let out = crate::claude_runner::filter_tool_defs(defs(), Some(&denied));
        let names: Vec<&str> = out.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, vec!["memory_search"]);

        // Qualified allowlist keeps the bare-named tool (previously dropped
        // every MCP tool for API-mode agents).
        let allowed = CapabilitiesConfig {
            allowed_tools: vec!["mcp__duduclaw__office_script".into()],
            ..Default::default()
        };
        let out = crate::claude_runner::filter_tool_defs(defs(), Some(&allowed));
        let names: Vec<&str> = out.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, vec!["office_script"]);

        // Degenerate prefix `mcp__` with no tool segment must not match everything.
        let weird = CapabilitiesConfig {
            denied_tools: vec!["mcp__duduclaw__".into()],
            ..Default::default()
        };
        let out = crate::claude_runner::filter_tool_defs(defs(), Some(&weird));
        assert_eq!(out.len(), 2, "empty tool segment must deny nothing");
    }

    // ── UsageTap + run_tool_loop (mock provider/executor, no HTTP) ──────────

    /// Scripted provider: replays canned responses (each carrying a usage
    /// stamp) and records how many times it was called.
    struct ScriptedProvider {
        script: Mutex<std::collections::VecDeque<ChatResponse>>,
        calls: Mutex<usize>,
    }
    impl ScriptedProvider {
        fn new(responses: Vec<ChatResponse>) -> Self {
            Self {
                script: Mutex::new(responses.into_iter().collect()),
                calls: Mutex::new(0),
            }
        }
    }
    #[async_trait]
    impl ChatProvider for ScriptedProvider {
        fn id(&self) -> &str {
            "scripted"
        }
        async fn complete(&self, _req: &ChatRequest) -> Result<ChatResponse, LlmError> {
            *self.calls.lock().unwrap() += 1;
            let mut s = self.script.lock().unwrap();
            if s.len() > 1 {
                Ok(s.pop_front().unwrap())
            } else {
                Ok(s.front().cloned().unwrap())
            }
        }
        async fn stream(
            &self,
            _req: &ChatRequest,
        ) -> Result<BoxStream<'static, Result<StreamEvent, LlmError>>, LlmError> {
            Err(LlmError::InvalidRequest("unused".into()))
        }
    }

    struct OneToolExecutor;
    #[async_trait]
    impl ToolExecutor for OneToolExecutor {
        fn defs(&self) -> Vec<ToolDef> {
            vec![ToolDef {
                name: "memory_search".into(),
                description: "search".into(),
                input_schema: serde_json::json!({"type": "object"}),
            }]
        }
        async fn call(&self, _name: &str, _args: serde_json::Value) -> Result<ToolOutcome, String> {
            Ok(ToolOutcome::ok("tool result payload"))
        }
    }

    fn resp(parts: Vec<ContentPart>, stop: StopReason, input: u64, output: u64) -> ChatResponse {
        ChatResponse {
            parts,
            stop,
            usage: NormalizedUsage {
                input_tokens: input,
                output_tokens: output,
                ..Default::default()
            },
            model_used: "m".into(),
            provider: "scripted".into(),
        }
    }

    // ── resolve_provider_config: secret:// convergence (coordinator #1) ────

    /// Every env var `resolve_provider_config`'s steps 1-3 consult, ahead of
    /// `[[accounts]]` in `config.toml` — guarded so a real key on the host
    /// running the test suite can never short-circuit before the account
    /// array under test is even reached.
    fn provider_env_guard_vars() -> [&'static str; 6] {
        PROVIDERS
            .iter()
            .map(|p| -> &'static str {
                match p.name {
                    "minimax" => "MINIMAX_API_KEY",
                    "deepseek" => "DEEPSEEK_API_KEY",
                    "openrouter" => "OPENROUTER_API_KEY",
                    "groq" => "GROQ_API_KEY",
                    "openai" => "OPENAI_API_KEY",
                    "xai" => "XAI_API_KEY",
                    other => panic!("unmapped provider preset in test guard: {other}"),
                }
            })
            .collect::<Vec<_>>()
            .try_into()
            .unwrap()
    }

    /// Removes every provider-env-var short-circuit for the duration of
    /// `f`, restoring the host's original values afterward (even on panic).
    async fn with_provider_env_guarded<F, Fut, T>(f: F) -> T
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = T>,
    {
        let vars = provider_env_guard_vars();
        let saved: Vec<(&str, Option<String>)> =
            vars.iter().map(|v| (*v, std::env::var(v).ok())).collect();
        // SAFETY: test-local env mutation; every value is restored below
        // (including on the panic path, via a drop guard).
        struct Restore(Vec<(&'static str, Option<String>)>);
        impl Drop for Restore {
            fn drop(&mut self) {
                for (v, val) in &self.0 {
                    unsafe {
                        match val {
                            Some(s) => std::env::set_var(v, s),
                            None => std::env::remove_var(v),
                        }
                    }
                }
            }
        }
        let _restore = Restore(saved);
        unsafe {
            for v in vars {
                std::env::remove_var(v);
            }
        }
        f().await
    }

    /// The dialect-8 bug this fix kills: before convergence onto `SecretRef`,
    /// an `api_key = "secret://vault/…"` reference with no `_enc` twin fell
    /// straight into the plaintext branch and was returned **verbatim** —
    /// exactly the credential that would then ride out as the `Authorization`
    /// bearer token to the vendor. With no `[secret_manager]` vault token
    /// configured, resolution must fail closed (no account found), and above
    /// all the literal `secret://` string must never surface anywhere in the
    /// outcome.
    #[tokio::test]
    async fn secret_reference_never_becomes_the_bearer_token_literal() {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(
            home.path().join("config.toml"),
            "[[accounts]]\nprovider = \"test-vault-provider\"\n\
             api_key = \"secret://vault/openai-compat-test\"\n",
        )
        .unwrap();

        let result =
            with_provider_env_guarded(|| resolve_provider_config(home.path(), "tester", None))
                .await;

        match &result {
            Ok((key, url, provider)) => panic!(
                "must not resolve without a configured vault backend — got a literal \
                 credential instead: key={key:?} url={url:?} provider={provider:?}"
            ),
            Err(e) => {
                assert!(
                    !e.contains("secret://"),
                    "error text must not leak the reference either: {e}"
                );
            }
        }
    }

    /// The other half of the fix: a reference a `SecretRef` sync-local
    /// backend genuinely *can* satisfy (`secret://env/…`) must actually
    /// resolve to the real value — this is a convergence onto the shared
    /// resolver, not a blanket new fail-closed regression.
    #[tokio::test]
    async fn secret_env_reference_resolves_to_the_real_key() {
        let home = tempfile::tempdir().unwrap();
        let var = format!("DUDUCLAW_OACOMPAT_TEST_{}", std::process::id());
        // SAFETY: process-unique env var name, set and removed within this test.
        unsafe { std::env::set_var(&var, "resolved-provider-key") };
        std::fs::write(
            home.path().join("config.toml"),
            format!(
                "[[accounts]]\nprovider = \"test-env-provider\"\n\
                 base_url = \"https://example.test/v1\"\n\
                 api_key = \"secret://env/{var}\"\n"
            ),
        )
        .unwrap();

        let result =
            with_provider_env_guarded(|| resolve_provider_config(home.path(), "tester", None))
                .await;
        // SAFETY: matches the set_var above.
        unsafe { std::env::remove_var(&var) };

        let (key, url, provider) = result.expect("env-backed secret:// reference must resolve");
        assert_eq!(key, "resolved-provider-key");
        assert_eq!(url, "https://example.test/v1");
        assert_eq!(provider, "test-env-provider");
    }

    /// Parity check: an `_enc` twin still round-trips through the keyfile
    /// exactly as before the convergence (WP-H1 P0 acceptance bar).
    #[tokio::test]
    async fn encrypted_api_key_still_round_trips() {
        let home = tempfile::tempdir().unwrap();
        let enc = crate::config_crypto::encrypt_value("enc-provider-key", home.path())
            .expect("encrypt_value");
        std::fs::write(
            home.path().join("config.toml"),
            format!(
                "[[accounts]]\nprovider = \"test-enc-provider\"\n\
                 base_url = \"https://example.test/v1\"\n\
                 api_key_enc = \"{enc}\"\n"
            ),
        )
        .unwrap();

        let result =
            with_provider_env_guarded(|| resolve_provider_config(home.path(), "tester", None))
                .await;

        let (key, ..) = result.expect("_enc field must still resolve");
        assert_eq!(key, "enc-provider-key");
    }

    #[tokio::test]
    async fn usage_tap_accumulates_tokens_across_tool_loop_rounds() {
        // Round 1: model asks for a tool (no answer text). Round 2: final text.
        // A tool-only intermediate round must NOT be mistaken for empty.
        let provider = UsageTap::new(ScriptedProvider::new(vec![
            resp(
                vec![ContentPart::ToolCall {
                    id: "c1".into(),
                    name: "memory_search".into(),
                    args: serde_json::json!({"q": "x"}),
                }],
                StopReason::ToolUse,
                100,
                20,
            ),
            resp(
                vec![ContentPart::Text("final answer".into())],
                StopReason::EndTurn,
                130,
                8,
            ),
        ]));
        let exec = OneToolExecutor;
        let mut req = ChatRequest::new("deepseek/deepseek-chat");
        req.messages
            .push(duduclaw_llm::ChatMessage::user("question"));

        let out = run_tool_loop(&provider, req, &exec, DEFAULT_MAX_TOOL_ITERS)
            .await
            .expect("loop ok");

        // Loop terminated on the text turn — not empty.
        let text = classify_final_text(&out, "deepseek-chat").expect("non-empty");
        assert_eq!(text, "final answer");
        assert_eq!(out.stop, StopReason::EndTurn);

        // Tokens accumulated across BOTH provider calls (100+130 in, 20+8 out).
        let total = provider.total();
        assert_eq!(total.input_tokens, 230);
        assert_eq!(total.output_tokens, 28);
    }
}
