//! OpenAI-compatible HTTP backend — works with Exo, llamafile, vLLM, SGLang, etc.
//!
//! # O11 (2026-09-29): one compat client, not two
//!
//! This module used to carry its own reqwest client, its own request/response
//! structs and its own SSE-less `chat/completions` call — a second
//! implementation of what `duduclaw-llm::providers::openai_compat` already
//! does, kept alive by one capability the shared provider lacked: per-token
//! **logprob** capture, which the UCCI calibrated cascade needs.
//!
//! That capability now lives in the shared provider
//! ([`OpenAiCompatProvider::complete_with_logprobs`]), so the HTTP half of this
//! file is gone. What stays here is the part that is genuinely local-inference's
//! own: the [`InferenceBackend`] contract (bookkeeping `load_model`, throughput
//! accounting) and the **interpretation** of logprobs — mean logprob and the
//! UCCI top-2 margin. The `ucci` dependency stays on this side of the seam on
//! purpose: `duduclaw-llm` transports a signal, it does not score one.
//!
//! Three local-specific settings are carried across explicitly rather than
//! inherited, because the shared provider's defaults are tuned for hosted APIs:
//! a 300s request timeout (CPU generation regularly exceeds two minutes),
//! verbatim model ids (a local server's `qwen/qwen3-4b` is a name, not a
//! `provider/model` qualifier), and the `X-DuDuClaw-Prefix-Hash` header.
//!
//! ## Prefix Caching Compatibility
//!
//! SGLang: RadixAttention automatically caches KV for shared prefixes.
//!   - Ensure system prompt is byte-identical across requests.
//!   - No special configuration needed beyond --enable-prefix-caching.
//! vLLM: Automatic Prefix Caching (APC) enabled via --enable-prefix-caching.
//!   - System prompt must be byte-identical including whitespace.
//! Both engines benefit from DuDuClaw's frozen SystemPromptSnapshot.

use async_trait::async_trait;
use duduclaw_llm::providers::{ChoiceLogprobs, OpenAiCompatProvider};
use duduclaw_llm::{
    ApiAuth, ChatMessage as LlmChatMessage, ChatRequest as LlmChatRequest, ContentPart, Role,
    SystemBlock,
};
use tokio::sync::RwLock;

use crate::backend::InferenceBackend;
use crate::config::OpenAiCompatConfig;
use crate::error::{InferenceError, Result};
use crate::types::*;

/// Custom header for monitoring prompt cache hit rates with SGLang/vLLM.
///
/// Set this to a hash of the system prompt content so that external monitoring
/// can correlate cache effectiveness across requests. SGLang RadixAttention
/// and vLLM APC automatically cache matching prefixes; this header enables
/// observability without affecting inference behavior.
const PREFIX_HASH_HEADER: &str = "X-DuDuClaw-Prefix-Hash";

/// Request timeout for a local completion. Deliberately far above the shared
/// `duduclaw-llm` 120s singleton: CPU generation of a few hundred tokens on a
/// small box regularly runs past two minutes, and a timeout there reads to the
/// router as an unavailable backend.
const LOCAL_REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);

/// Backend that calls an OpenAI-compatible HTTP API.
pub struct OpenAiCompatBackend {
    config: OpenAiCompatConfig,
    /// Probe client for `GET /models` (`is_available`). The completion path
    /// runs through the shared provider below.
    probe_client: reqwest::Client,
    loaded_model: RwLock<Option<ModelInfo>>,
    models_url: String,
    /// Resolved API key (decrypted from `api_key_enc` or plaintext `api_key`).
    /// Resolved once at construction — read-only / fail-soft. `None` means no
    /// Authorization header is sent (same as today's empty-key behaviour).
    resolved_api_key: Option<String>,
    /// Optional system prompt content hash for cache monitoring.
    prefix_hash: Option<String>,
}

/// Resolve the DuDuClaw home dir, honouring `DUDUCLAW_HOME` (multi-instance
/// isolation). Delegates to the canonical [`duduclaw_core::duduclaw_home`] so
/// this crate can't drift back to a hardcoded `~/.duduclaw`.
fn default_duduclaw_home() -> std::path::PathBuf {
    duduclaw_core::duduclaw_home()
}

/// The `ModelInfo` an HTTP backend reports for its configured model.
///
/// Deliberately honest about what is unknown: an OpenAI-compatible server
/// exposes a name, not an architecture, a parameter count, a quantization
/// or a file size, so those stay `"unknown"` / `0` rather than being
/// guessed. `context_length` is the conservative 4096 floor every server in
/// this class supports; callers that need the real window ask the server.
fn remote_model_info(config: &OpenAiCompatConfig) -> ModelInfo {
    ModelInfo {
        id: config.model.clone(),
        path: config.base_url.clone(),
        architecture: "remote".to_string(),
        parameter_count: "unknown".to_string(),
        quantization: "unknown".to_string(),
        file_size_bytes: 0,
        estimated_memory_mb: 0,
        kv_cache_mb: 0, // remote — managed by server
        is_loaded: true,
        context_length: 4096,
    }
}

impl OpenAiCompatBackend {
    /// Construct using the standard `~/.duduclaw` home dir for key resolution.
    pub async fn new(config: OpenAiCompatConfig) -> Self {
        let home = default_duduclaw_home();
        Self::new_with_home(config, &home).await
    }

    /// Construct, resolving `api_key_enc` against an explicit `home_dir`.
    ///
    /// Async since WP-6C — both call sites (`InferenceManager::init` /
    /// `create_backend`) already run on the crate's tokio runtime, so key
    /// resolution can now reach a network-backed `secret://` reference
    /// instead of failing closed on it.
    pub async fn new_with_home(config: OpenAiCompatConfig, home_dir: &std::path::Path) -> Self {
        let probe_client = reqwest::Client::builder()
            .timeout(LOCAL_REQUEST_TIMEOUT)
            .build()
            .unwrap_or_default();

        let base = config.base_url.trim_end_matches('/');
        let models_url = format!("{base}/models");

        let resolved_api_key = config.resolved_api_key(home_dir).await;

        // WP-D: an HTTP backend has nothing to load — the server already
        // holds the weights, and `load_model` below is a pure bookkeeping
        // no-op that records the configured name. Seeding it here is what
        // makes a bare `[openai_compat]` section usable on its own.
        //
        // Before this, `InferenceEngine::generate` refused with
        // `NoModelLoaded` whenever the request carried no `model_id` AND
        // `default_model` was unset — a live, reachable server answering
        // "No model loaded". That is exactly the appliance's shape (the
        // image's llama.cpp server is configured by default; nobody writes
        // a `default_model`), and it also hit every hand-written
        // `[openai_compat]` config that omitted `default_model`.
        //
        // An empty `model` stays `None`: that config names no model, so
        // claiming one would be the fabrication this change exists to stop.
        let loaded_model = (!config.model.trim().is_empty()).then(|| remote_model_info(&config));

        Self {
            config,
            probe_client,
            loaded_model: RwLock::new(loaded_model),
            models_url,
            resolved_api_key,
            prefix_hash: None,
        }
    }

    /// Set the system prompt content hash for cache monitoring.
    ///
    /// SGLang RadixAttention and vLLM APC automatically cache matching prefixes;
    /// this header enables external monitoring of cache effectiveness.
    /// The hash is sent as an `X-DuDuClaw-Prefix-Hash` header on every request.
    pub fn with_prefix_hash(mut self, hash: &str) -> Self {
        self.prefix_hash = Some(hash.to_string());
        self
    }

    /// Build the shared compat provider for this backend's endpoint.
    ///
    /// Constructed per call rather than stored: it is a handful of `String`s
    /// plus a client handle (`reqwest::Client` is an `Arc` internally), and a
    /// stored provider would need the same interior mutability the rest of
    /// this struct avoids.
    fn provider(&self) -> OpenAiCompatProvider {
        let auth = ApiAuth {
            api_key: self.resolved_api_key.clone().unwrap_or_default(),
            base_url: Some(self.config.base_url.clone()),
        };
        let mut p = OpenAiCompatProvider::new("local", auth, self.config.base_url.clone())
            // A local server's model id is a name, never a `provider/model`
            // qualifier — see `with_verbatim_model_id`.
            .with_verbatim_model_id()
            .with_timeout(LOCAL_REQUEST_TIMEOUT);
        if let Some(hash) = &self.prefix_hash {
            p = p.with_header(PREFIX_HASH_HEADER, hash.as_str());
        }
        p
    }
}

/// Mean per-token logprob of a choice, `None` when the server returned no
/// logprobs (or an empty token list) — post-hoc confidence then stays off
/// (fail-safe: identical behaviour to a server without logprob support).
fn mean_logprob_of(lp: &ChoiceLogprobs) -> Option<f32> {
    if lp.tokens.is_empty() {
        return None;
    }
    let sum: f64 = lp.tokens.iter().map(|t| t.logprob).sum();
    Some((sum / lp.tokens.len() as f64) as f32)
}

/// UCCI top-2 margin uncertainty over the choice's content tokens.
///
/// `None` when any token is missing its candidate list (the signal is only
/// meaningful over a complete sequence) or when dropping the reported stop
/// token would leave nothing to measure.
fn margin_uncertainty_of(lp: &ChoiceLogprobs, drop_stop_token: bool) -> Option<f64> {
    let tokens = if drop_stop_token && lp.finish_reason.as_deref() == Some("stop") {
        lp.tokens.get(..lp.tokens.len().checked_sub(1)?)?
    } else {
        &lp.tokens[..]
    };
    if tokens.is_empty() {
        return None;
    }
    let candidates: Vec<Vec<f64>> = tokens
        .iter()
        .map(|token| token.top_logprobs.clone())
        .collect::<Option<_>>()?;
    ucci::signal::uncertainty_from_top_logprobs(&candidates).ok()
}

#[async_trait]
impl InferenceBackend for OpenAiCompatBackend {
    fn name(&self) -> &str {
        "openai-compat"
    }

    fn requires_local_file(&self) -> bool {
        false
    }

    async fn load_model(&self, _model_path: &str, _params: &GenerationParams) -> Result<ModelInfo> {
        // HTTP backends manage their own models — this only records the name.
        let info = remote_model_info(&self.config);
        *self.loaded_model.write().await = Some(info.clone());
        Ok(info)
    }

    async fn unload_model(&self) -> Result<()> {
        *self.loaded_model.write().await = None;
        Ok(())
    }

    async fn loaded_model(&self) -> Option<ModelInfo> {
        self.loaded_model.read().await.clone()
    }

    async fn generate(&self, request: &InferenceRequest) -> Result<InferenceResponse> {
        let start = std::time::Instant::now();

        let model = request.model_id.as_deref().unwrap_or(&self.config.model);

        let mut chat = LlmChatRequest::new(model);
        if !request.system_prompt.is_empty() {
            chat.system
                .push(SystemBlock::uncached(&request.system_prompt));
        }
        chat.messages.push(LlmChatMessage {
            role: Role::User,
            parts: vec![ContentPart::Text(request.user_prompt.clone())],
        });
        chat.max_tokens = request.params.max_tokens;
        chat.temperature = Some(request.params.temperature);
        chat.top_p = Some(request.params.top_p);
        chat.stop = request.params.stop.clone();
        chat.logprobs = request.params.capture_logprobs.then_some(true);
        chat.top_logprobs = request.params.capture_top_logprobs.then_some(2);

        let (resp, logprobs) = self
            .provider()
            .complete_with_logprobs(&chat)
            .await
            .map_err(|e| InferenceError::Http(e.to_string()))?;

        let text = resp.text();
        let mean_logprob = logprobs.as_ref().and_then(mean_logprob_of);
        let margin_uncertainty = logprobs
            .as_ref()
            .and_then(|lp| margin_uncertainty_of(lp, request.params.ucci_drop_stop_token));

        let elapsed = start.elapsed();
        // `NormalizedUsage` subtracts cache reads out of `input_tokens`; a
        // local server reports neither, so adding them back is a no-op there
        // and keeps the number honest if a caching proxy is in front.
        let tokens_prompt = (resp.usage.input_tokens + resp.usage.cache_read_tokens) as u32;
        let tokens_generated = resp.usage.output_tokens as u32;

        let tps = if elapsed.as_millis() > 0 {
            tokens_generated as f64 / elapsed.as_secs_f64()
        } else {
            0.0
        };

        Ok(InferenceResponse {
            text,
            tokens_generated,
            tokens_prompt,
            generation_time_ms: elapsed.as_millis() as u64,
            tokens_per_second: tps,
            backend: BackendType::OpenAiCompat,
            model_id: model.to_string(),
            mean_logprob,
            margin_uncertainty,
        })
    }

    async fn is_available(&self) -> bool {
        let url = &self.models_url;
        let mut req = self.probe_client.get(url);
        if let Some(ref key) = self.resolved_api_key {
            req = req.bearer_auth(key);
        }
        matches!(req.send().await, Ok(r) if r.status().is_success())
    }
}

#[cfg(test)]
mod logprob_tests {
    use super::*;
    use duduclaw_llm::providers::TokenLogprob;

    fn lp(tokens: Vec<(f64, Option<Vec<f64>>)>, finish: Option<&str>) -> ChoiceLogprobs {
        ChoiceLogprobs {
            tokens: tokens
                .into_iter()
                .map(|(logprob, top_logprobs)| TokenLogprob {
                    logprob,
                    top_logprobs,
                })
                .collect(),
            finish_reason: finish.map(str::to_string),
        }
    }

    #[test]
    fn mean_logprob_parsed_from_openai_response() {
        let mean = mean_logprob_of(&lp(vec![(-0.2, None), (-0.4, None)], None)).expect("mean");
        assert!((mean - (-0.3)).abs() < 1e-6);
    }

    #[test]
    fn top_two_logprobs_produce_ucci_margin_uncertainty() {
        let signal = lp(
            vec![
                (
                    -0.10536051565782628,
                    Some(vec![-0.10536051565782628, -2.302585092994046]),
                ),
                (
                    -0.6931471805599453,
                    Some(vec![-0.6931471805599453, -1.3862943611198906]),
                ),
            ],
            None,
        );
        let u = margin_uncertainty_of(&signal, false).unwrap();
        assert!((u - 0.475).abs() < 1e-12);
    }

    #[test]
    fn configured_stop_token_is_excluded_from_ucci_signal() {
        let signal = lp(
            vec![
                (-0.1, Some(vec![-0.10536051565782628, -2.302585092994046])),
                (-0.69, Some(vec![-0.6931471805599453, -1.3862943611198906])),
            ],
            Some("stop"),
        );
        let u = margin_uncertainty_of(&signal, true).unwrap();
        assert!((u - 0.2).abs() < 1e-12);
    }

    #[test]
    fn missing_logprobs_yields_none() {
        // Server that ignores the logprobs field (fail-safe path): the shared
        // provider hands back `None` and nothing downstream is scored.
        let body: serde_json::Value = serde_json::from_str(
            r#"{"choices":[{"message":{"role":"assistant","content":"hi"}}],"usage":null}"#,
        )
        .unwrap();
        assert!(duduclaw_llm::providers::openai_compat::parse_logprobs(&body).is_none());
    }

    #[test]
    fn empty_logprob_content_yields_none() {
        let signal = lp(vec![], None);
        assert!(mean_logprob_of(&signal).is_none());
        assert!(margin_uncertainty_of(&signal, false).is_none());
    }

    /// O11 regression: a request that does not ask for logprobs must leave the
    /// wire body exactly as a legacy server expects it (no `logprobs` key).
    #[test]
    fn logprobs_field_omitted_from_request_when_not_captured() {
        let mut chat = LlmChatRequest::new("m");
        chat.max_tokens = 10;
        chat.temperature = Some(0.7);
        chat.top_p = Some(0.9);
        let json = serde_json::to_string(
            &duduclaw_llm::providers::openai_compat::build_request_body(&chat, false),
        )
        .unwrap();
        assert!(
            !json.contains("logprobs"),
            "legacy request body must be unchanged: {json}"
        );

        chat.logprobs = Some(true);
        let json = serde_json::to_string(
            &duduclaw_llm::providers::openai_compat::build_request_body(&chat, false),
        )
        .unwrap();
        assert!(json.contains("\"logprobs\":true"));
        assert!(!json.contains("top_logprobs"));
    }
}

#[cfg(test)]
mod loaded_model_tests {
    use super::*;
    use crate::backend::InferenceBackend;

    fn cfg(model: &str) -> OpenAiCompatConfig {
        OpenAiCompatConfig {
            base_url: "http://127.0.0.1:8080/v1".into(),
            api_key: None,
            api_key_enc: None,
            model: model.into(),
        }
    }

    /// WP-D regression: a configured HTTP endpoint is usable on its own.
    ///
    /// `InferenceEngine::generate` refuses with `NoModelLoaded` unless the
    /// backend reports a loaded model, and for an HTTP backend "loading" is
    /// pure bookkeeping. Before the constructor seeded it, a bare
    /// `[openai_compat]` section with no `default_model` — the appliance's
    /// exact shape — made a live, reachable server answer "No model loaded".
    #[tokio::test]
    async fn constructor_reports_the_configured_model_as_loaded() {
        let home = tempfile::tempdir().unwrap();
        let backend = OpenAiCompatBackend::new_with_home(cfg("local"), home.path()).await;
        let loaded = backend
            .loaded_model()
            .await
            .expect("model reported as loaded");
        assert_eq!(loaded.id, "local");
        assert_eq!(loaded.path, "http://127.0.0.1:8080/v1");
        // Nothing is guessed about weights we cannot see.
        assert_eq!(loaded.parameter_count, "unknown");
        assert_eq!(loaded.file_size_bytes, 0);
    }

    /// A config naming no model must not have one invented for it.
    #[tokio::test]
    async fn an_empty_model_name_stays_unloaded() {
        let home = tempfile::tempdir().unwrap();
        let backend = OpenAiCompatBackend::new_with_home(cfg("   "), home.path()).await;
        assert!(backend.loaded_model().await.is_none());
    }

    #[tokio::test]
    async fn unload_still_clears_it() {
        let home = tempfile::tempdir().unwrap();
        let backend = OpenAiCompatBackend::new_with_home(cfg("local"), home.path()).await;
        backend.unload_model().await.unwrap();
        assert!(backend.loaded_model().await.is_none());
    }

    /// O11: the local endpoint must reach the server under its own name even
    /// when that name contains a slash (HuggingFace repo ids), and the chat
    /// URL must still be `<base>/chat/completions`.
    #[tokio::test]
    async fn local_provider_keeps_slashed_model_ids_and_base_url() {
        let home = tempfile::tempdir().unwrap();
        let backend = OpenAiCompatBackend::new_with_home(cfg("qwen/qwen3-4b"), home.path()).await;
        let provider = backend.provider();
        assert_eq!(provider.base_url(), "http://127.0.0.1:8080/v1");
        let mut chat = LlmChatRequest::new("qwen/qwen3-4b");
        chat.max_tokens = 8;
        let body = duduclaw_llm::providers::openai_compat::build_request_body_with(&chat, false, true);
        assert_eq!(body["model"], serde_json::json!("qwen/qwen3-4b"));
    }
}
