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

    /// LLMLingua-2 prompt compression settings
    pub llmlingua: Option<crate::compression::llmlingua::LlmLinguaConfig>,

    /// StreamingLLM session window settings
    pub streaming_llm: Option<crate::compression::streaming_llm::StreamingLlmConfig>,
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
            llmlingua: None,
            streaming_llm: None,
        }
    }
}

/// Configuration for OpenAI-compatible HTTP backend.
#[derive(Clone, Serialize, Deserialize)]
pub struct OpenAiCompatConfig {
    /// Base URL (e.g., "http://localhost:8080/v1")
    pub base_url: String,
    /// API key (if required)
    pub api_key: Option<String>,
    /// Model name to request
    pub model: String,
}

impl std::fmt::Debug for OpenAiCompatConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenAiCompatConfig")
            .field("base_url", &self.base_url)
            .field("api_key", &self.api_key.as_ref().map(|_| "[REDACTED]"))
            .field("model", &self.model)
            .finish()
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
        }
    }
}

impl InferenceConfig {
    /// Load config from `~/.duduclaw/inference.toml`, falling back to defaults.
    pub async fn load(home_dir: &Path) -> Self {
        let config_path = home_dir.join("inference.toml");
        match tokio::fs::read_to_string(&config_path).await {
            Ok(content) => match toml::from_str(&content) {
                Ok(config) => config,
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
        if let Some(ref compat) = self.openai_compat {
            if compat.base_url.is_empty() {
                return Err(InferenceError::Config(
                    "openai_compat.base_url cannot be empty".to_string(),
                ));
            }
        }
        if let Some(ref router) = self.router {
            if router.enabled && router.fast_threshold <= router.strong_threshold {
                return Err(InferenceError::Config(
                    "router.fast_threshold must be > router.strong_threshold".to_string(),
                ));
            }
        }
        Ok(())
    }
}
