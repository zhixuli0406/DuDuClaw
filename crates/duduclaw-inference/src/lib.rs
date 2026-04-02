//! Local LLM inference engine for DuDuClaw.
//!
//! Provides a unified `InferenceBackend` trait with pluggable backends:
//! - **llama.cpp** (via `llama-cpp-2` crate): Metal / CUDA / Vulkan / CPU
//! - **mistral.rs** (via `mistralrs-core`): Rust-native, ISQ, PagedAttention, Speculative Decoding
//! - **OpenAI-compatible HTTP** (for Exo, llamafile, vLLM, etc.)
//!
//! Multi-mode inference with automatic failover:
//!   Exo Cluster → llamafile → Direct Backend → OpenAI-compat → Cloud API
//!
//! The **ConfidenceRouter** routes queries to the best tier:
//!   LocalFast (small model) → LocalStrong (large model) → Cloud API
//!
//! **MLX Bridge** enables local evolution reflections on Apple Silicon.
//!
//! **Compression** module provides three strategies:
//!   Meta-Token (lossless) / LLMLingua-2 (lossy) / StreamingLLM (KV-cache)

pub mod backend;
pub mod compression;
pub mod config;
pub mod engine;
pub mod error;
pub mod exo_cluster;
pub mod hardware;
#[cfg(any(feature = "metal", feature = "cuda", feature = "vulkan"))]
pub mod llama_cpp;
pub mod llamafile;
pub mod manager;
#[cfg(feature = "mistralrs")]
pub mod mistral_rs;
pub mod mlx_bridge;
pub mod model_manager;
pub mod model_registry;
pub mod openai_compat;
pub mod router;
pub mod types;
pub mod util;
pub mod asr;
pub mod asr_router;
pub mod audio_decode;
pub mod deepgram;
#[cfg(feature = "onnx")]
pub mod sensevoice;
pub mod livekit_voice;
pub mod realtime_voice;
pub mod vad;
pub mod whisper;

pub use backend::InferenceBackend;
pub use config::InferenceConfig;
pub use engine::InferenceEngine;
pub use error::InferenceError;
pub use manager::{InferenceManager, InferenceMode};
pub use router::ConfidenceRouter;
pub use types::*;
