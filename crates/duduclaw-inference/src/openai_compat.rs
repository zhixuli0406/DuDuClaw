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
    /// Optional system prompt content hash for cache monitoring.
    prefix_hash: Option<String>,
}

impl OpenAiCompatBackend {
    pub fn new(config: OpenAiCompatConfig) -> Self {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(300))
            .build()
            .unwrap_or_default();

        let base = config.base_url.trim_end_matches('/');
        let chat_url = format!("{base}/chat/completions");
        let models_url = format!("{base}/models");

        Self {
            config,
            client,
            loaded_model: RwLock::new(None),
            chat_url,
            models_url,
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
        };

        let url = &self.chat_url;

        let mut req = self.client.post(url).json(&body);
        if let Some(ref key) = self.config.api_key {
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

        let text = chat_resp
            .choices
            .first()
            .map(|c| c.message.content.clone())
            .unwrap_or_default();

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
        })
    }

    async fn is_available(&self) -> bool {
        let url = &self.models_url;
        let mut req = self.client.get(url);
        if let Some(ref key) = self.config.api_key {
            req = req.bearer_auth(key);
        }
        matches!(req.send().await, Ok(r) if r.status().is_success())
    }
}
