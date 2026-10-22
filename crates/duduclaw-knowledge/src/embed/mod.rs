//! Vector embeddings.
//!
//! [`Embedder`] is implemented by a local ONNX model
//! ([`local_onnx::LocalOnnxEmbedder`], models pinned in [`catalog`]) and by
//! the cloud providers ([`cloud::CloudEmbedder`], text de-identified by a
//! [`redactor::TextRedactor`] first). When no embedder is usable,
//! [`NoEmbedder`] / [`UnavailableEmbedder`] say why, and retrieval runs on
//! keywords only with that reason shown — never silently, and never by
//! switching to another provider.
//!
//! | module | role |
//! |---|---|
//! | [`catalog`] | pinned local models (`embed-catalog.json`) |
//! | [`install`] | download / offline verify / remove a local model |
//! | [`job`] | compute a source's missing vectors (reuse first) |
//! | [`local_onnx`] | ONNX Runtime inference, e5 mean / Qwen3 last-token pooling |
//! | [`pooling`] | pooling and vector math |
//! | [`cloud`], [`cloud_openai`], [`cloud_gemini`] | cloud providers |
//! | [`redactor`] | de-identification seam for cloud calls |
//! | [`config`] | `[knowledge.embedding]` |
//! | [`vectors`] | vector rows: pending, reuse by `text_sha256`, coverage |
//! | [`vector_cache`] | brute-force cosine search over one model's vectors |

pub mod catalog;
pub mod cloud;
pub mod cloud_gemini;
pub mod cloud_openai;
pub mod config;
pub mod install;
pub mod job;
pub mod local_onnx;
pub mod pooling;
pub mod redactor;
pub mod vector_cache;
pub mod vectors;

/// Where embedding inputs go.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Egress {
    /// Computed on this machine.
    Local,
    /// Sent to a provider (after de-identification).
    Cloud { provider: String, host: String },
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EmbedError {
    /// No usable model: not installed, unsupported platform, failed to load.
    #[error("{0}")]
    Unavailable(String),
    #[error("{0}")]
    Failed(String),
}

/// A text embedding model. Vectors of different `model_id`s are never
/// compared.
pub trait Embedder: Send + Sync {
    /// Stored with every vector, e.g. `local/multilingual-e5-small/int8@rev`.
    fn model_id(&self) -> &str;
    fn dim(&self) -> usize;
    fn max_input_tokens(&self) -> usize;
    fn embed_passages(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, EmbedError>;
    /// The query form (models add their own prefix or instruction).
    fn embed_query(&self, text: &str) -> Result<Vec<f32>, EmbedError>;
    fn egress(&self) -> Egress;
}

/// zh-TW reason shown wherever retrieval falls back to keywords only
/// because no model is installed.
pub const KEYWORD_ONLY_REASON: &str = "尚未安裝向量模型";

/// An embedder that cannot run, carrying the zh-TW reason (not installed,
/// unsupported platform, no disclosure acknowledgement, redaction off, …).
#[derive(Debug, Clone)]
pub struct UnavailableEmbedder {
    pub reason: String,
}

impl UnavailableEmbedder {
    pub fn new(reason: impl Into<String>) -> Self {
        Self { reason: reason.into() }
    }
}

impl Embedder for UnavailableEmbedder {
    fn model_id(&self) -> &str {
        "none"
    }

    fn dim(&self) -> usize {
        0
    }

    fn max_input_tokens(&self) -> usize {
        0
    }

    fn embed_passages(&self, _texts: &[&str]) -> Result<Vec<Vec<f32>>, EmbedError> {
        Err(EmbedError::Unavailable(self.reason.clone()))
    }

    fn embed_query(&self, _text: &str) -> Result<Vec<f32>, EmbedError> {
        Err(EmbedError::Unavailable(self.reason.clone()))
    }

    fn egress(&self) -> Egress {
        Egress::Local
    }
}

/// Whether `e` can produce vectors at all (cheap; no inference).
pub fn is_available(e: &dyn Embedder) -> bool {
    e.dim() > 0
}

/// The "no vector model" embedder.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoEmbedder;

impl Embedder for NoEmbedder {
    fn model_id(&self) -> &str {
        "none"
    }

    fn dim(&self) -> usize {
        0
    }

    fn max_input_tokens(&self) -> usize {
        0
    }

    fn embed_passages(&self, _texts: &[&str]) -> Result<Vec<Vec<f32>>, EmbedError> {
        Err(EmbedError::Unavailable(KEYWORD_ONLY_REASON.to_string()))
    }

    fn embed_query(&self, _text: &str) -> Result<Vec<f32>, EmbedError> {
        Err(EmbedError::Unavailable(KEYWORD_ONLY_REASON.to_string()))
    }

    fn egress(&self) -> Egress {
        Egress::Local
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_embedder_is_unavailable() {
        let e = NoEmbedder;
        assert!(matches!(
            e.embed_query("x"),
            Err(EmbedError::Unavailable(_))
        ));
        assert!(matches!(
            e.embed_passages(&["x"]),
            Err(EmbedError::Unavailable(_))
        ));
        assert_eq!(e.egress(), Egress::Local);
    }
}
