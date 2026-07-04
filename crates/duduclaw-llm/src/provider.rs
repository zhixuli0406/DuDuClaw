//! `ChatProvider` trait — the API-level twin of the CLI-level
//! `AgentRuntime` trait — plus provider identity and credential plumbing.

use async_trait::async_trait;
use futures_util::stream::BoxStream;

use crate::error::LlmError;
use crate::types::{ChatRequest, ChatResponse, StreamEvent};

/// Well-known provider families. Compat providers (DeepSeek, Qwen, xAI,
/// Groq, ...) all speak the legacy chat-completions dialect and are
/// distinguished by their string id (see [`crate::providers::openai_compat`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProviderId {
    Anthropic,
    OpenAi,
    Gemini,
    /// Any OpenAI-compatible chat/completions endpoint.
    OpenAiCompat,
}

impl ProviderId {
    pub fn as_str(&self) -> &'static str {
        match self {
            ProviderId::Anthropic => "anthropic",
            ProviderId::OpenAi => "openai",
            ProviderId::Gemini => "gemini",
            ProviderId::OpenAiCompat => "openai_compat",
        }
    }
}

/// Credentials handed to a provider at construction. Resolution from
/// env/config/encrypted accounts happens in the gateway wiring (later
/// wave) — this crate only defines the container plus the standard
/// env-var fallback helper [`resolve_env_key`].
#[derive(Debug, Clone)]
pub struct ApiAuth {
    pub api_key: String,
    /// Override the provider's default base URL (proxies, self-hosted
    /// compat servers). `None` → provider default.
    pub base_url: Option<String>,
}

impl ApiAuth {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self { api_key: api_key.into(), base_url: None }
    }

    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = Some(base_url.into());
        self
    }
}

/// Standard environment-variable names per provider id.
///
/// Mirrors (and extends) the gateway's `runtime/openai_compat.rs` PROVIDERS
/// table so both layers agree on env names. Returns the first non-empty
/// matching variable.
pub fn resolve_env_key(provider_id: &str) -> Option<String> {
    let names: &[&str] = match provider_id {
        "anthropic" => &["ANTHROPIC_API_KEY"],
        "openai" => &["OPENAI_API_KEY"],
        "gemini" | "google" => &["GEMINI_API_KEY", "GOOGLE_API_KEY"],
        "deepseek" => &["DEEPSEEK_API_KEY"],
        "minimax" => &["MINIMAX_API_KEY"],
        "groq" => &["GROQ_API_KEY"],
        "together" => &["TOGETHER_API_KEY"],
        "mistral" => &["MISTRAL_API_KEY"],
        "openrouter" => &["OPENROUTER_API_KEY"],
        "xai" => &["XAI_API_KEY"],
        "qwen" => &["DASHSCOPE_API_KEY", "QWEN_API_KEY"],
        _ => return None,
    };
    names
        .iter()
        .filter_map(|n| std::env::var(n).ok())
        .find(|v| !v.is_empty())
}

/// Split a fully-qualified model id (`"anthropic/claude-sonnet-5"`) into
/// `(provider, bare_model)`. A bare id yields `(None, id)`.
///
/// Only the FIRST `/` splits — OpenRouter-style ids like
/// `"openrouter/anthropic/claude-sonnet-5"` keep the remainder intact.
pub fn split_model_id(model: &str) -> (Option<&str>, &str) {
    match model.split_once('/') {
        Some((provider, bare)) if !provider.is_empty() && !bare.is_empty() => {
            (Some(provider), bare)
        }
        _ => (None, model),
    }
}

/// Provider-agnostic chat completion interface.
///
/// Implementations translate the normalized [`ChatRequest`] to their native
/// wire format and back. All request-building and response-parsing is pure
/// (exposed as `build_request_body` / `parse_response` per provider module)
/// so translation is testable offline.
#[async_trait]
pub trait ChatProvider: Send + Sync {
    /// Stable provider id: `"anthropic"`, `"openai"`, `"gemini"`, or a
    /// compat id like `"deepseek"` / `"openrouter"`.
    fn id(&self) -> &str;

    /// One-shot completion.
    async fn complete(&self, req: &ChatRequest) -> Result<ChatResponse, LlmError>;

    /// Streaming completion. Yields deltas and terminates with exactly one
    /// [`StreamEvent::Done`]. Providers without native SSE support in v1
    /// (OpenAI Responses, Gemini — documented per module) implement this as
    /// `complete()` followed by a single `Done` event.
    async fn stream(
        &self,
        req: &ChatRequest,
    ) -> Result<BoxStream<'static, Result<StreamEvent, LlmError>>, LlmError>;
}

/// Fallback implementation of [`ChatProvider::stream`] for buffered
/// providers: run `complete()` and emit one `Done` event.
pub(crate) fn buffered_stream(
    resp: ChatResponse,
) -> BoxStream<'static, Result<StreamEvent, LlmError>> {
    Box::pin(futures_util::stream::once(async move {
        Ok(StreamEvent::Done(resp))
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_model_id_qualified_and_bare() {
        assert_eq!(
            split_model_id("anthropic/claude-sonnet-5"),
            (Some("anthropic"), "claude-sonnet-5")
        );
        assert_eq!(split_model_id("claude-sonnet-5"), (None, "claude-sonnet-5"));
        // Only first slash splits.
        assert_eq!(
            split_model_id("openrouter/anthropic/claude-sonnet-5"),
            (Some("openrouter"), "anthropic/claude-sonnet-5")
        );
        // Degenerate forms stay bare.
        assert_eq!(split_model_id("/x"), (None, "/x"));
        assert_eq!(split_model_id("x/"), (None, "x/"));
    }

    #[test]
    fn resolve_env_key_unknown_provider_is_none() {
        assert_eq!(resolve_env_key("no-such-provider"), None);
    }

    #[test]
    fn provider_id_strings() {
        assert_eq!(ProviderId::Anthropic.as_str(), "anthropic");
        assert_eq!(ProviderId::OpenAiCompat.as_str(), "openai_compat");
    }
}
