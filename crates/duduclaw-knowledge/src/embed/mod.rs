//! Vector embedding seam.
//!
//! This release ships keyword retrieval only. [`Embedder`] is the interface a
//! local (ONNX) or cloud provider implements later; [`NoEmbedder`] is the
//! only implementation here and reports why vector retrieval is off, so every
//! caller can say so instead of silently searching less.

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

/// zh-TW reason shown wherever retrieval falls back to keywords only.
pub const KEYWORD_ONLY_REASON: &str = "尚未安裝向量模型";

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
        assert!(matches!(e.embed_query("x"), Err(EmbedError::Unavailable(_))));
        assert!(matches!(e.embed_passages(&["x"]), Err(EmbedError::Unavailable(_))));
        assert_eq!(e.egress(), Egress::Local);
    }
}
