//! Local LLM inference engine for DuDuClaw.
//!
//! Provides a unified `InferenceBackend` trait. The one shipped backend is
//! **OpenAI-compatible HTTP** (llama-server, Ollama, vLLM, SGLang, llamafile…).
//! The in-process llama.cpp / mistral.rs / MLX backends were removed on
//! 2026-09-29 (`wiki/reports/feature-audit-2026-09-29.md` T1-D2/D3, T3-S5):
//! none of them was ever compiled into a shipped binary.
//!
//! Multi-mode inference with automatic failover:
//!   llamafile → Direct Backend → OpenAI-compat → Cloud API
//!
//! The **ConfidenceRouter** routes queries to the best tier:
//!   LocalFast (small model) → LocalStrong (large model) → Cloud API,
//!   with [`ucci`] as the one calibrated escalation gate.

pub mod adapter;
pub mod appliance;
pub mod backend;
pub mod config;
pub mod engine;
pub mod error;
pub mod hardware;
pub mod llamafile;
pub mod manager;
pub mod model_manager;
pub mod model_registry;
pub mod openai_compat;
pub mod router;
pub mod types;
pub mod ucci;
pub mod util;
pub mod embedding;
pub mod whisper;

pub use adapter::CompatEndpoint;
pub use backend::InferenceBackend;
pub use config::InferenceConfig;
pub use engine::InferenceEngine;
pub use error::InferenceError;
pub use manager::{InferenceManager, InferenceMode};
pub use router::ConfidenceRouter;
pub use types::*;
