//! OpenAI-compatible HTTP backend — works with Exo, llamafile, vLLM, SGLang, etc.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

use crate::backend::InferenceBackend;
use crate::config::OpenAiCompatConfig;
use crate::error::{InferenceError, Result};
use crate::types::*;

// ── Prefix Caching Compatibility ──────────────────────────────
// SGLang: RadixAttention automatically caches KV for shared prefixes.
//   - Ensure system prompt is byte-identical across requests.
//   - No special configuration needed beyond --enable-prefix-caching.
// vLLM: Automatic Prefix Caching (APC) enabled via --enable-prefix-caching.
//   - System prompt must be byte-identical including whitespace.
// Both engines benefit from DuDuClaw's frozen SystemPromptSnapshot.
// ──────────────────────────────────────────────────────────────

/// Custom header for monitoring prompt cache hit rates with SGLang/vLLM.
///
/// Set this to a hash of the system prompt content so that external monitoring
/// can correlate cache effectiveness across requests. SGLang RadixAttention
/// and vLLM APC automatically cache matching prefixes; this header enables
/// observability without affecting inference behavior.
const PREFIX_HASH_HEADER: &str = "X-DuDuClaw-Prefix-Hash";

/// Backend that calls an OpenAI-compatible HTTP API.
pub struct OpenAiCompatBackend {
    config: OpenAiCompatConfig,
    client: reqwest::Client,
    loaded_model: RwLock<Option<ModelInfo>>,
    chat_url: String,
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

impl OpenAiCompatBackend {
    /// Construct using the standard `~/.duduclaw` home dir for key resolution.
    pub fn new(config: OpenAiCompatConfig) -> Self {
        let home = default_duduclaw_home();
        Self::new_with_home(config, &home)
    }

    /// Construct, resolving `api_key_enc` against an explicit `home_dir`.
    pub fn new_with_home(config: OpenAiCompatConfig, home_dir: &std::path::Path) -> Self {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(300))
            .build()
            .unwrap_or_default();

        let base = config.base_url.trim_end_matches('/');
        let chat_url = format!("{base}/chat/completions");
        let models_url = format!("{base}/models");

        let resolved_api_key = config.resolved_api_key(home_dir);

        Self {
            config,
            client,
            loaded_model: RwLock::new(None),
            chat_url,
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
}

// OpenAI API types (minimal subset)

#[derive(Serialize)]
struct ChatRequest {
    model: String,
    messages: Vec<ChatMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    top_p: Option<f32>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    stop: Vec<String>,
    /// Request per-token logprobs (supported by llama.cpp server, vLLM, SGLang).
    /// Omitted entirely when not requested so legacy servers see an unchanged body.
    #[serde(skip_serializing_if = "Option::is_none")]
    logprobs: Option<bool>,
}

#[derive(Serialize, Deserialize)]
struct ChatMessage {
    role: String,
    content: String,
}

#[derive(Deserialize)]
struct ChatResponse {
    choices: Vec<ChatChoice>,
    usage: Option<ChatUsage>,
}

#[derive(Deserialize)]
struct ChatChoice {
    message: ChatMessage,
    /// Present when the request set `logprobs: true` and the server supports it.
    #[serde(default)]
    logprobs: Option<ChoiceLogprobs>,
}

#[derive(Deserialize)]
struct ChoiceLogprobs {
    #[serde(default)]
    content: Option<Vec<TokenLogprob>>,
}

#[derive(Deserialize)]
struct TokenLogprob {
    logprob: f64,
}

/// Mean per-token logprob of a choice, `None` when the server returned no
/// logprobs (or an empty token list) — post-hoc confidence then stays off
/// (fail-safe: identical behaviour to a server without logprob support).
fn mean_logprob_of(choice: &ChatChoice) -> Option<f32> {
    let tokens = choice.logprobs.as_ref()?.content.as_deref()?;
    if tokens.is_empty() {
        return None;
    }
    let sum: f64 = tokens.iter().map(|t| t.logprob).sum();
    Some((sum / tokens.len() as f64) as f32)
}

#[derive(Deserialize)]
struct ChatUsage {
    prompt_tokens: u32,
    completion_tokens: u32,
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
        // HTTP backends manage their own models — just verify connectivity
        let info = ModelInfo {
            id: self.config.model.clone(),
            path: self.config.base_url.clone(),
            architecture: "remote".to_string(),
            parameter_count: "unknown".to_string(),
            quantization: "unknown".to_string(),
            file_size_bytes: 0,
            estimated_memory_mb: 0,
            kv_cache_mb: 0, // remote — managed by server
            is_loaded: true,
            context_length: 4096,
        };
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

        let mut messages = Vec::new();
        if !request.system_prompt.is_empty() {
            messages.push(ChatMessage {
                role: "system".to_string(),
                content: request.system_prompt.clone(),
            });
        }
        messages.push(ChatMessage {
            role: "user".to_string(),
            content: request.user_prompt.clone(),
        });

        let model = request
            .model_id
            .as_deref()
            .unwrap_or(&self.config.model);

        let body = ChatRequest {
            model: model.to_string(),
            messages,
            max_tokens: Some(request.params.max_tokens),
            temperature: Some(request.params.temperature),
            top_p: Some(request.params.top_p),
            stop: request.params.stop.clone(),
            logprobs: request.params.capture_logprobs.then_some(true),
        };

        let url = &self.chat_url;

        let mut req = self.client.post(url).json(&body);
        if let Some(ref key) = self.resolved_api_key {
            req = req.bearer_auth(key);
        }
        if let Some(ref hash) = self.prefix_hash {
            req = req.header(PREFIX_HASH_HEADER, hash.as_str());
        }

        let resp = req.send().await.map_err(|e| InferenceError::Http(e.to_string()))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text: String = resp.text().await.unwrap_or_default().chars().take(200).collect();
            return Err(InferenceError::Http(format!("HTTP {status}: {text}")));
        }

        let chat_resp: ChatResponse = resp
            .json()
            .await
            .map_err(|e| InferenceError::Http(format!("Failed to parse response: {e}")))?;

        let first_choice = chat_resp.choices.first();
        let text = first_choice
            .map(|c| c.message.content.clone())
            .unwrap_or_default();
        let mean_logprob = first_choice.and_then(mean_logprob_of);

        let elapsed = start.elapsed();
        let usage = chat_resp.usage.unwrap_or(ChatUsage {
            prompt_tokens: 0,
            completion_tokens: 0,
        });

        let tps = if elapsed.as_millis() > 0 {
            usage.completion_tokens as f64 / elapsed.as_secs_f64()
        } else {
            0.0
        };

        Ok(InferenceResponse {
            text,
            tokens_generated: usage.completion_tokens,
            tokens_prompt: usage.prompt_tokens,
            generation_time_ms: elapsed.as_millis() as u64,
            tokens_per_second: tps,
            backend: BackendType::OpenAiCompat,
            model_id: model.to_string(),
            mean_logprob,
        })
    }

    async fn is_available(&self) -> bool {
        let url = &self.models_url;
        let mut req = self.client.get(url);
        if let Some(ref key) = self.resolved_api_key {
            req = req.bearer_auth(key);
        }
        matches!(req.send().await, Ok(r) if r.status().is_success())
    }
}

#[cfg(test)]
mod logprob_tests {
    use super::*;

    #[test]
    fn mean_logprob_parsed_from_openai_response() {
        let json = r#"{
            "choices": [{
                "message": {"role": "assistant", "content": "hi"},
                "logprobs": {"content": [
                    {"token": "h", "logprob": -0.2},
                    {"token": "i", "logprob": -0.4}
                ]}
            }],
            "usage": {"prompt_tokens": 3, "completion_tokens": 2}
        }"#;
        let resp: ChatResponse = serde_json::from_str(json).expect("parse");
        let mean = mean_logprob_of(resp.choices.first().unwrap()).expect("mean");
        assert!((mean - (-0.3)).abs() < 1e-6);
    }

    #[test]
    fn missing_logprobs_yields_none() {
        // Server that ignores the logprobs field (fail-safe path).
        let json = r#"{
            "choices": [{"message": {"role": "assistant", "content": "hi"}}],
            "usage": null
        }"#;
        let resp: ChatResponse = serde_json::from_str(json).expect("parse");
        assert!(mean_logprob_of(resp.choices.first().unwrap()).is_none());
    }

    #[test]
    fn empty_logprob_content_yields_none() {
        let json = r#"{
            "choices": [{
                "message": {"role": "assistant", "content": ""},
                "logprobs": {"content": []}
            }]
        }"#;
        let resp: ChatResponse = serde_json::from_str(json).expect("parse");
        assert!(mean_logprob_of(resp.choices.first().unwrap()).is_none());
    }

    #[test]
    fn logprobs_field_omitted_from_request_when_not_captured() {
        let body = ChatRequest {
            model: "m".into(),
            messages: vec![],
            max_tokens: Some(10),
            temperature: Some(0.7),
            top_p: Some(0.9),
            stop: vec![],
            logprobs: None,
        };
        let json = serde_json::to_string(&body).unwrap();
        assert!(!json.contains("logprobs"), "legacy request body must be unchanged: {json}");

        let body = ChatRequest { logprobs: Some(true), ..body };
        let json = serde_json::to_string(&body).unwrap();
        assert!(json.contains("\"logprobs\":true"));
    }
}
