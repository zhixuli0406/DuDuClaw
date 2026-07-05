//! Inference configuration — read from `~/.duduclaw/inference.toml`.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::InferenceError;
use crate::types::{BackendType, GenerationParams};

/// Top-level inference configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct InferenceConfig {
    /// Whether local inference is enabled
    pub enabled: bool,

    /// Preferred backend (auto-detected if not set)
    pub backend: Option<BackendType>,

    /// Directory for GGUF model files
    pub models_dir: String,

    /// Default model to load on startup (model id / filename)
    pub default_model: Option<String>,

    /// Default generation parameters
    pub generation: GenerationParams,

    /// Auto-load default model on gateway startup
    pub auto_load: bool,

    /// Maximum memory budget for inference in MB (0 = unlimited)
    pub max_memory_mb: u64,

    /// OpenAI-compatible endpoint (for Exo, llamafile, vLLM, etc.)
    pub openai_compat: Option<OpenAiCompatConfig>,

    /// mistral.rs specific settings
    pub mistralrs: Option<MistralRsConfig>,

    /// Confidence router settings (three-tier routing)
    pub router: Option<RouterConfig>,

    /// Exo P2P cluster settings
    pub exo: Option<crate::exo_cluster::ExoConfig>,

    /// llamafile subprocess settings
    pub llamafile: Option<crate::llamafile::LlamafileConfig>,

    /// MLX bridge settings (Apple Silicon evolution)
    pub mlx: Option<crate::mlx_bridge::MlxConfig>,

    /// Voice / ASR / TTS settings
    pub voice: Option<VoiceConfig>,

    /// Embedding model settings for semantic similarity in the prediction engine.
    ///
    /// ```toml
    /// [embedding]
    /// enabled = true
    /// model = "bge-small-zh"
    /// auto_download = true
    /// max_history = 100
    /// ```
    pub embedding: Option<EmbeddingConfig>,
}

/// Embedding model configuration for the prediction engine.
///
/// Default model: BGE-small-zh-v1.5 (33M params, 512-dim, INT8 ONNX ~24MB).
/// Minimum hardware: +128MB RAM, +25MB disk. No GPU required.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct EmbeddingConfig {
    /// Enable embedding-based prediction (requires `onnx` feature).
    pub enabled: bool,
    /// Model identifier: "bge-small-zh" (default) or "qwen3-embedding-0.6b"
    pub model: String,
    /// Custom model directory (default: ~/.duduclaw/models/embedding/)
    pub model_dir: Option<String>,
    /// Auto-download model on first use from HuggingFace
    pub auto_download: bool,
    /// Maximum embedding history per user-agent pair (rolling window)
    pub max_history: usize,
    /// ONNX intra-op thread count (default: auto, capped at 4)
    pub threads: Option<usize>,
}

impl Default for EmbeddingConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            model: "bge-small-zh".to_string(),
            model_dir: None,
            auto_download: true,
            max_history: 100,
            threads: None,
        }
    }
}

/// Voice pipeline configuration — ASR + TTS + language settings.
///
/// Configured in `inference.toml` under `[voice]`:
/// ```toml
/// [voice]
/// asr_provider = "auto"       # "auto" | "whisper-api" | "whisper-local"
/// tts_provider = "auto"       # "auto" | "edge-tts" | "minimax" | "openai-tts" | "piper"
/// asr_language = "zh"         # BCP-47 language hint
/// tts_voice = ""              # Empty = auto-detect from text content
/// voice_reply_enabled = false # Enable voice reply by default (overridable via /voice)
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct VoiceConfig {
    /// ASR provider selection: "auto", "whisper-api", "whisper-local"
    pub asr_provider: String,
    /// TTS provider selection: "auto", "edge-tts", "minimax", "openai-tts", "piper"
    pub tts_provider: String,
    /// Default ASR language hint (BCP-47)
    pub asr_language: String,
    /// Default TTS voice name (empty = auto-detect from text content)
    pub tts_voice: String,
    /// Enable voice reply mode by default for all sessions
    pub voice_reply_enabled: bool,
}

impl Default for VoiceConfig {
    fn default() -> Self {
        Self {
            asr_provider: "auto".into(),
            tts_provider: "auto".into(),
            asr_language: "zh".into(),
            tts_voice: String::new(),
            voice_reply_enabled: false,
        }
    }
}

impl VoiceConfig {
    /// Allowed ASR provider values.
    const VALID_ASR_PROVIDERS: &[&str] = &["auto", "whisper-api", "whisper-local"];
    /// Allowed TTS provider values.
    const VALID_TTS_PROVIDERS: &[&str] = &["auto", "edge-tts", "minimax", "openai-tts", "piper"];

    /// Validate and normalize config values, falling back to "auto" for unknown providers.
    pub fn validate(&mut self) {
        if !Self::VALID_ASR_PROVIDERS.contains(&self.asr_provider.as_str()) {
            tracing::warn!(provider = %self.asr_provider, "Unknown ASR provider, falling back to auto");
            self.asr_provider = "auto".into();
        }
        if !Self::VALID_TTS_PROVIDERS.contains(&self.tts_provider.as_str()) {
            tracing::warn!(provider = %self.tts_provider, "Unknown TTS provider, falling back to auto");
            self.tts_provider = "auto".into();
        }
        // Sanitize language to alphanumeric + hyphen
        self.asr_language = self.asr_language.chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
            .collect();
        if self.asr_language.is_empty() {
            self.asr_language = "zh".into();
        }
    }
}

impl Default for InferenceConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            backend: None,
            models_dir: "~/.duduclaw/models".to_string(),
            default_model: None,
            generation: GenerationParams::default(),
            auto_load: false,
            max_memory_mb: 0,
            openai_compat: None,
            mistralrs: None,
            router: None,
            exo: None,
            llamafile: None,
            mlx: None,
            voice: None,
            embedding: None,
        }
    }
}

/// Configuration for OpenAI-compatible HTTP backend.
#[derive(Clone, Serialize, Deserialize)]
pub struct OpenAiCompatConfig {
    /// Base URL (e.g., "http://localhost:8080/v1")
    pub base_url: String,
    /// API key (if required), plaintext form.
    pub api_key: Option<String>,
    /// AES-256-GCM encrypted API key (base64), written by the gateway.
    /// Preferred over `api_key` when set — see [`OpenAiCompatConfig::resolved_api_key`].
    #[serde(default)]
    pub api_key_enc: Option<String>,
    /// Model name to request
    pub model: String,
}

impl std::fmt::Debug for OpenAiCompatConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenAiCompatConfig")
            .field("base_url", &self.base_url)
            .field("api_key", &self.api_key.as_ref().map(|_| "[REDACTED]"))
            .field("api_key_enc", &self.api_key_enc.as_ref().map(|_| "[REDACTED]"))
            .field("model", &self.model)
            .finish()
    }
}

impl OpenAiCompatConfig {
    /// Resolve the effective API key, read-only / fail-soft.
    ///
    /// If `api_key_enc` is set + non-empty, decrypt it via the per-machine
    /// keyfile (`~/.duduclaw/.keyfile`). On any decrypt failure (missing/short
    /// keyfile, bad ciphertext) this falls back to the plaintext `api_key`.
    /// Returns `None` when neither yields a non-empty key — callers should then
    /// behave as today's "no key" case (no Authorization header).
    pub fn resolved_api_key(&self, home_dir: &std::path::Path) -> Option<String> {
        if let Some(enc) = self.api_key_enc.as_deref() {
            if !enc.is_empty() {
                if let Some(plain) =
                    duduclaw_security::keyfile::decrypt_keyfile_value(enc, home_dir)
                {
                    return Some(plain);
                }
            }
        }
        self.api_key.clone().filter(|s| !s.is_empty())
    }
}

/// Configuration for mistral.rs backend.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct MistralRsConfig {
    /// ISQ quantization bits (2, 3, 4, 5, 6, 8, or null for native precision).
    /// In-Situ Quantization: loads safetensors and quantizes on-the-fly.
    pub isq_bits: Option<u8>,

    /// Enable PagedAttention for KV-cache management.
    pub paged_attention: bool,

    /// Enable speculative decoding.
    pub speculative: Option<SpeculativeConfig>,
}

impl Default for MistralRsConfig {
    fn default() -> Self {
        Self {
            isq_bits: Some(4),
            paged_attention: true,
            speculative: None,
        }
    }
}

/// Speculative decoding configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpeculativeConfig {
    /// Speculative decoding method: "draft" (EAGLE-style) or "self" (LayerSkip).
    pub method: SpeculativeMethod,

    /// Draft model path (for "draft" method).
    /// Should be a small, fast model (e.g., 0.6B-1B params).
    pub draft_model: Option<String>,

    /// Number of speculative tokens to generate per step (default 5).
    #[serde(default = "default_spec_tokens")]
    pub num_tokens: u32,
}

/// Speculative decoding method.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpeculativeMethod {
    /// Use a separate draft model (EAGLE-2 style).
    Draft,
    /// Self-speculative: use early layers of the target model as draft.
    #[serde(rename = "self")]
    SelfSpec,
}

fn default_spec_tokens() -> u32 {
    5
}

/// Confidence router configuration — routes queries to the best tier.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RouterConfig {
    /// Enable the confidence router.
    pub enabled: bool,

    /// Confidence threshold: above this → LocalFast tier.
    pub fast_threshold: f32,

    /// Confidence threshold: above this → LocalStrong tier.
    /// Below this → Cloud API tier.
    pub strong_threshold: f32,

    /// Fast tier model id (small, fast local model).
    /// e.g., "gemma-3-4b-q4_k_m" or "qwen3-0.6b-q8_0"
    pub fast_model: Option<String>,

    /// Strong tier model id (large, capable local model).
    /// e.g., "qwen3-8b-q4_k_m" or "deepseek-v3-lite-16b-q4_k_m"
    pub strong_model: Option<String>,

    /// Maximum token count in prompt before escalating to a higher tier.
    pub max_fast_prompt_tokens: u32,

    /// Keywords that always escalate to Cloud API (e.g., complex reasoning).
    #[serde(default)]
    pub cloud_keywords: Vec<String>,

    /// Keywords that stay at LocalFast tier (simple queries).
    #[serde(default)]
    pub fast_keywords: Vec<String>,

    /// Enable post-hoc (cascade) confidence: after a local tier answers, the
    /// mean token logprob is Platt-scaled into an acceptance probability and
    /// low-confidence answers escalate to the next tier instead of being
    /// returned. Zero LLM cost — the logprob signal comes with the response.
    /// (Cascade Routing arXiv:2410.10347; UCCI arXiv:2605.18796)
    #[serde(default)]
    pub post_hoc_enabled: bool,

    /// Platt scaling slope: g = sigmoid(alpha * p̄ + beta), p̄ = exp(mean logprob).
    #[serde(default = "default_post_hoc_alpha")]
    pub post_hoc_alpha: f32,

    /// Platt scaling intercept.
    #[serde(default = "default_post_hoc_beta")]
    pub post_hoc_beta: f32,

    /// Acceptance threshold on g: below this the answer escalates.
    #[serde(default = "default_post_hoc_accept_threshold")]
    pub post_hoc_accept_threshold: f32,

    /// Allow the embedding host (gateway) to run its MCP tool loop against
    /// the local OpenAI-compatible endpoint. Absent (`None`) defaults to
    /// **enabled** when the active backend is OpenAI-compat — most local
    /// servers (llamafile, vLLM, SGLang, Ollama) handle OpenAI tool JSON.
    /// Small models may still emit malformed tool calls; the gateway's tool
    /// loop feeds those back to the model fail-soft rather than aborting.
    /// Set `local_tools = false` to force bare completions.
    #[serde(default)]
    pub local_tools: Option<bool>,
}

fn default_post_hoc_alpha() -> f32 {
    4.0
}

fn default_post_hoc_beta() -> f32 {
    -2.0
}

fn default_post_hoc_accept_threshold() -> f32 {
    0.5
}

impl Default for RouterConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            fast_threshold: 0.7,
            strong_threshold: 0.35,
            fast_model: None,
            strong_model: None,
            max_fast_prompt_tokens: 1000,
            // Only keywords that truly require Cloud API reasoning.
            // Removed "review", "complex" — LocalStrong can handle these.
            cloud_keywords: vec![
                "refactor".to_string(),
                "architect".to_string(),
                "security audit".to_string(),
                "multi-step".to_string(),
            ],
            // Expanded: more query types that local models handle well.
            fast_keywords: vec![
                // English
                "translate".to_string(),
                "summarize".to_string(),
                "classify".to_string(),
                "format".to_string(),
                "explain".to_string(),
                "hello".to_string(),
                "hi".to_string(),
                "define".to_string(),
                "list".to_string(),
                "convert".to_string(),
                "count".to_string(),
                "extract".to_string(),
                "rewrite".to_string(),
                // CJK / zh-TW common patterns
                "翻譯".to_string(),
                "摘要".to_string(),
                "分類".to_string(),
                "格式".to_string(),
                "解釋".to_string(),
                "你好".to_string(),
                "定義".to_string(),
                "列出".to_string(),
                "轉換".to_string(),
                "改寫".to_string(),
            ],
            post_hoc_enabled: false,
            post_hoc_alpha: default_post_hoc_alpha(),
            post_hoc_beta: default_post_hoc_beta(),
            post_hoc_accept_threshold: default_post_hoc_accept_threshold(),
            local_tools: None,
        }
    }
}

impl InferenceConfig {
    /// Load config from `~/.duduclaw/inference.toml`, falling back to defaults.
    pub async fn load(home_dir: &Path) -> Self {
        let config_path = home_dir.join("inference.toml");
        match tokio::fs::read_to_string(&config_path).await {
            Ok(content) => match toml::from_str::<Self>(&content) {
                Ok(mut config) => {
                    // Auto-validate voice config on load
                    if let Some(ref mut voice) = config.voice {
                        voice.validate();
                    }
                    config
                }
                Err(e) => {
                    tracing::warn!("Failed to parse inference.toml: {e}, using defaults");
                    Self::default()
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(e) => {
                tracing::warn!("Failed to read inference.toml: {e}, using defaults");
                Self::default()
            }
        }
    }

    /// Resolve the models directory, expanding `~`.
    pub fn models_path(&self) -> PathBuf {
        crate::util::expand_tilde(&self.models_dir)
    }

    /// Validate configuration.
    pub fn validate(&self) -> crate::error::Result<()> {
        if let Some(ref compat) = self.openai_compat
            && compat.base_url.is_empty() {
                return Err(InferenceError::Config(
                    "openai_compat.base_url cannot be empty".to_string(),
                ));
            }
        if let Some(ref router) = self.router
            && router.enabled && router.fast_threshold <= router.strong_threshold {
                return Err(InferenceError::Config(
                    "router.fast_threshold must be > router.strong_threshold".to_string(),
                ));
            }
        if let Some(ref router) = self.router
            && router.post_hoc_enabled
            && !(0.0..=1.0).contains(&router.post_hoc_accept_threshold) {
                return Err(InferenceError::Config(
                    "router.post_hoc_accept_threshold must be within [0.0, 1.0]".to_string(),
                ));
            }
        Ok(())
    }
}

#[cfg(test)]
mod openai_compat_key_tests {
    use super::OpenAiCompatConfig;
    use duduclaw_security::crypto::CryptoEngine;

    use std::sync::atomic::{AtomicU64, Ordering};
    static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

    struct TempHome(std::path::PathBuf);
    impl TempHome {
        fn new() -> Self {
            let n = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
            let p = std::env::temp_dir().join(format!(
                "duduclaw-inference-keytest-{}-{}",
                std::process::id(),
                n
            ));
            std::fs::create_dir_all(&p).unwrap();
            Self(p)
        }
        fn path(&self) -> &std::path::Path {
            &self.0
        }
        fn with_keyfile(&self) -> CryptoEngine {
            let key = CryptoEngine::generate_key().unwrap();
            std::fs::write(self.0.join(".keyfile"), key).unwrap();
            CryptoEngine::new(&key).unwrap()
        }
    }
    impl Drop for TempHome {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn cfg(api_key: Option<&str>, api_key_enc: Option<&str>) -> OpenAiCompatConfig {
        OpenAiCompatConfig {
            base_url: "http://localhost:8080/v1".into(),
            api_key: api_key.map(str::to_string),
            api_key_enc: api_key_enc.map(str::to_string),
            model: "test-model".into(),
        }
    }

    #[test]
    fn enc_key_resolves_to_plaintext() {
        let home = TempHome::new();
        let engine = home.with_keyfile();
        let enc = engine.encrypt_string("sk-encrypted").unwrap();
        let c = cfg(Some("sk-plain"), Some(&enc));
        assert_eq!(c.resolved_api_key(home.path()).as_deref(), Some("sk-encrypted"));
    }

    #[test]
    fn plaintext_only_returns_plaintext() {
        let home = TempHome::new();
        let c = cfg(Some("sk-plain"), None);
        assert_eq!(c.resolved_api_key(home.path()).as_deref(), Some("sk-plain"));
    }

    #[test]
    fn neither_returns_none() {
        let home = TempHome::new();
        let c = cfg(None, None);
        assert!(c.resolved_api_key(home.path()).is_none());
    }

    #[test]
    fn enc_failure_falls_back_to_plaintext() {
        // No keyfile present → cannot decrypt enc → fall back to plaintext.
        let home = TempHome::new();
        let c = cfg(Some("sk-plain"), Some("garbage"));
        assert_eq!(c.resolved_api_key(home.path()).as_deref(), Some("sk-plain"));
    }

    #[test]
    fn debug_redacts_both_key_fields() {
        let c = cfg(Some("sk-plain"), Some("enc-blob"));
        let dbg = format!("{c:?}");
        assert!(!dbg.contains("sk-plain"));
        assert!(!dbg.contains("enc-blob"));
        assert!(dbg.contains("[REDACTED]"));
    }
}
