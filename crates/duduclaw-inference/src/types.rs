//! Core types for local inference.

use serde::{Deserialize, Serialize};

/// Which backend engine to use for inference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackendType {
    /// llama.cpp via llama-cpp-2 crate (Metal/CUDA/Vulkan/CPU)
    LlamaCpp,
    /// OpenAI-compatible HTTP server (Exo, llamafile, vLLM, etc.)
    OpenAiCompat,
    /// mistral.rs native Rust engine (future)
    MistralRs,
}

impl std::fmt::Display for BackendType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::LlamaCpp => write!(f, "llama.cpp"),
            Self::OpenAiCompat => write!(f, "openai-compat"),
            Self::MistralRs => write!(f, "mistral.rs"),
        }
    }
}

/// GPU accelerator type detected on the system.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GpuType {
    AppleSilicon,
    NvidiaCuda,
    AmdRocm,
    IntelArc,
    Vulkan,
    None,
}

/// Information about a loaded or available model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelInfo {
    /// Unique identifier (filename without extension)
    pub id: String,
    /// Full path to model file
    pub path: String,
    /// Model architecture (e.g., "llama", "qwen2", "gemma")
    pub architecture: String,
    /// Parameter count (e.g., "8B", "72B")
    pub parameter_count: String,
    /// Quantization type (e.g., "Q4_K_M", "Q8_0")
    pub quantization: String,
    /// File size in bytes
    pub file_size_bytes: u64,
    /// Estimated VRAM/RAM required in MB
    pub estimated_memory_mb: u64,
    /// Whether the model is currently loaded
    pub is_loaded: bool,
    /// Context length supported
    pub context_length: u32,
}

/// Parameters for text generation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GenerationParams {
    /// Maximum tokens to generate
    #[serde(default = "default_max_tokens")]
    pub max_tokens: u32,
    /// Sampling temperature (0.0 = greedy, 1.0 = creative)
    #[serde(default = "default_temperature")]
    pub temperature: f32,
    /// Top-p (nucleus) sampling
    #[serde(default = "default_top_p")]
    pub top_p: f32,
    /// Stop sequences
    #[serde(default)]
    pub stop: Vec<String>,
    /// Number of GPU layers to offload (-1 = all)
    #[serde(default = "default_gpu_layers")]
    pub gpu_layers: i32,
    /// Context window size
    #[serde(default = "default_context_size")]
    pub context_size: u32,
}

fn default_max_tokens() -> u32 {
    2048
}
fn default_temperature() -> f32 {
    0.7
}
fn default_top_p() -> f32 {
    0.9
}
fn default_gpu_layers() -> i32 {
    -1
}
fn default_context_size() -> u32 {
    4096
}

impl Default for GenerationParams {
    fn default() -> Self {
        Self {
            max_tokens: default_max_tokens(),
            temperature: default_temperature(),
            top_p: default_top_p(),
            stop: Vec::new(),
            gpu_layers: default_gpu_layers(),
            context_size: default_context_size(),
        }
    }
}

/// Request to generate text.
#[derive(Debug, Clone)]
pub struct InferenceRequest {
    /// System prompt (agent persona / instructions)
    pub system_prompt: String,
    /// User prompt (the actual query)
    pub user_prompt: String,
    /// Generation parameters
    pub params: GenerationParams,
    /// Which model to use (id from ModelInfo)
    pub model_id: Option<String>,
}

/// Response from text generation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InferenceResponse {
    /// Generated text
    pub text: String,
    /// Tokens generated
    pub tokens_generated: u32,
    /// Tokens in prompt
    pub tokens_prompt: u32,
    /// Generation time in milliseconds
    pub generation_time_ms: u64,
    /// Tokens per second
    pub tokens_per_second: f64,
    /// Which backend was used
    pub backend: BackendType,
    /// Which model was used
    pub model_id: String,
}

/// Hardware information detected on the system.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HardwareInfo {
    /// GPU type
    pub gpu_type: GpuType,
    /// GPU name (e.g., "Apple M4 Max", "NVIDIA RTX 4090")
    pub gpu_name: String,
    /// Total VRAM/unified memory in MB
    pub vram_total_mb: u64,
    /// Available VRAM/unified memory in MB
    pub vram_available_mb: u64,
    /// Total system RAM in MB
    pub ram_total_mb: u64,
    /// Available system RAM in MB
    pub ram_available_mb: u64,
    /// Number of CPU cores
    pub cpu_cores: u32,
    /// Recommended backend
    pub recommended_backend: BackendType,
    /// Recommended max model size in GB
    pub recommended_max_model_gb: f64,
}
