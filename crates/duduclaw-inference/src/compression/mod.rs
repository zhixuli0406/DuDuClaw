//! Token and prompt compression for DuDuClaw.
//!
//! Three compression strategies:
//! - **Meta-Token (LTSC)**: Lossless — replace repeated multi-token subsequences with
//!   single meta-tokens. Best for structured/repetitive input (JSON, code, templates).
//! - **LLMLingua-2**: Lossy — token-level importance pruning via a lightweight classifier.
//!   Best for natural language prompt compression (2-5x ratio, minimal quality loss).
//! - **StreamingLLM**: KV-cache management — attention sink + sliding window for
//!   infinite-length generation without OOM.

pub mod meta_token;
pub mod llmlingua;
pub mod streaming_llm;

use serde::{Deserialize, Serialize};

/// Compression statistics returned after a compress operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompressionStats {
    /// Original length (chars or tokens depending on method)
    pub original_len: usize,
    /// Compressed length
    pub compressed_len: usize,
    /// Compression ratio (original / compressed)
    pub ratio: f64,
    /// Method used
    pub method: String,
    /// Whether the compression is lossless
    pub lossless: bool,
}

impl CompressionStats {
    pub fn new(original: usize, compressed: usize, method: &str, lossless: bool) -> Self {
        let ratio = if compressed > 0 {
            original as f64 / compressed as f64
        } else {
            f64::INFINITY
        };
        Self {
            original_len: original,
            compressed_len: compressed,
            ratio,
            method: method.to_string(),
            lossless,
        }
    }
}
