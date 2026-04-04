//! Text embedding provider for semantic similarity in the prediction engine.
//!
//! Provides a trait-based interface for embedding models. The gateway consumes
//! `Vec<f32>` embeddings without knowing the underlying ONNX runtime details.
//!
//! Default model: BGE-small-zh-v1.5 (33M params, 512-dim, INT8 ONNX ~24MB)
//!
//! ## Feature gating
//!
//! - `OnnxEmbeddingProvider`: behind `#[cfg(feature = "onnx")]`
//! - `NoopEmbeddingProvider`: always available (returns error)

use async_trait::async_trait;

use crate::error::InferenceError;

// ---------------------------------------------------------------------------
// Trait
// ---------------------------------------------------------------------------

/// Trait for text embedding providers.
///
/// Implementations must be `Send + Sync` for use behind `Arc<dyn EmbeddingProvider>`.
#[async_trait]
pub trait EmbeddingProvider: Send + Sync {
    /// Embed a single text string, returning a normalized vector.
    async fn embed(&self, text: &str) -> Result<Vec<f32>, InferenceError>;

    /// Embed a batch of texts (more efficient for ONNX).
    async fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, InferenceError>;

    /// Embedding dimension (e.g., 512 for BGE-small-zh).
    fn dimension(&self) -> usize;

    /// Provider name for logging.
    fn name(&self) -> &str;
}

// ---------------------------------------------------------------------------
// Noop fallback (always available)
// ---------------------------------------------------------------------------

/// Fallback provider that always returns an error.
/// Used when the ONNX feature is not compiled or no model is loaded.
pub struct NoopEmbeddingProvider;

#[async_trait]
impl EmbeddingProvider for NoopEmbeddingProvider {
    async fn embed(&self, _text: &str) -> Result<Vec<f32>, InferenceError> {
        Err(InferenceError::BackendUnavailable {
            backend: "embedding".into(),
            reason: "No embedding model loaded (onnx feature or model file missing)".into(),
        })
    }

    async fn embed_batch(&self, _texts: &[&str]) -> Result<Vec<Vec<f32>>, InferenceError> {
        Err(InferenceError::BackendUnavailable {
            backend: "embedding".into(),
            reason: "No embedding model loaded".into(),
        })
    }

    fn dimension(&self) -> usize {
        0
    }

    fn name(&self) -> &str {
        "noop"
    }
}

// ---------------------------------------------------------------------------
// ONNX implementation
// ---------------------------------------------------------------------------

#[cfg(feature = "onnx")]
mod onnx_impl {
    use std::path::Path;

    use ndarray::{Array2, Axis};
    use ort::session::Session;
    use tokenizers::Tokenizer;
    use tracing::info;

    use super::*;

    /// Embedding provider backed by ONNX Runtime + HuggingFace tokenizer.
    ///
    /// Loads a BERT-family embedding model (BGE, GTE, etc.) and its tokenizer.
    /// Performs mean pooling over token embeddings with attention mask weighting,
    /// then L2-normalizes the result.
    pub struct OnnxEmbeddingProvider {
        session: Session,
        tokenizer: Tokenizer,
        dimension: usize,
        model_name: String,
        max_length: usize,
    }

    impl OnnxEmbeddingProvider {
        /// Load an embedding model from a directory containing `model.onnx`
        /// (or `model_quantized.onnx`) and `tokenizer.json`.
        ///
        /// ```text
        /// ~/.duduclaw/models/embedding/bge-small-zh/
        ///     model.onnx          (or model_quantized.onnx)
        ///     tokenizer.json
        /// ```
        pub fn load(model_dir: &Path, model_name: &str) -> Result<Self, InferenceError> {
            // Validate path (no traversal)
            let dir_str = model_dir.to_string_lossy();
            if dir_str.contains("..") || dir_str.contains('\0') {
                return Err(InferenceError::Other(
                    "Model directory contains invalid path components".into(),
                ));
            }

            // Find ONNX model file (prefer quantized)
            let model_path = if model_dir.join("model_quantized.onnx").exists() {
                model_dir.join("model_quantized.onnx")
            } else if model_dir.join("model.onnx").exists() {
                model_dir.join("model.onnx")
            } else {
                return Err(InferenceError::ModelNotFound {
                    path: model_dir.join("model.onnx").to_string_lossy().into(),
                });
            };

            // Load tokenizer
            let tokenizer_path = model_dir.join("tokenizer.json");
            if !tokenizer_path.exists() {
                return Err(InferenceError::ModelNotFound {
                    path: tokenizer_path.to_string_lossy().into(),
                });
            }

            let tokenizer = Tokenizer::from_file(tokenizer_path.to_string_lossy().as_ref())
                .map_err(|e| {
                    InferenceError::Other(format!("Failed to load tokenizer: {e}"))
                })?;

            // Determine thread count (cap at 4 for lightweight embedding model)
            let threads = std::thread::available_parallelism()
                .map(|n| n.get().min(4))
                .unwrap_or(2);

            // Load ONNX session
            let session = Session::builder()
                .map_err(|e| InferenceError::Other(format!("ONNX session builder error: {e}")))?
                .with_optimization_level(ort::session::builder::GraphOptimizationLevel::Level3)
                .map_err(|e| InferenceError::Other(format!("ONNX optimization error: {e}")))?
                .with_intra_threads(threads)
                .map_err(|e| InferenceError::Other(format!("ONNX thread config error: {e}")))?
                .commit_from_file(&model_path)
                .map_err(|e| {
                    InferenceError::Other(format!(
                        "Failed to load ONNX model {}: {e}",
                        model_path.display()
                    ))
                })?;

            // Infer dimension from model name rather than ONNX output metadata.
            // ONNX output shape often has dynamic axes (None for batch/seq dims),
            // and the ort v2.0 Output enum structure varies across versions.
            // Known model dimensions are more reliable.
            let dimension = match model_name {
                "bge-small-zh" | "bge-small-zh-v1.5" => 512,
                "bge-base-zh" | "bge-base-zh-v1.5" => 768,
                "bge-m3" => 1024,
                "qwen3-embedding-0.6b" => 1024,
                "gte-small" => 512,
                "gte-base" => 768,
                _ => 512, // safe default for BERT-small family
            };

            info!(
                model = model_name,
                dimension,
                threads,
                path = %model_path.display(),
                "Loaded embedding model"
            );

            Ok(Self {
                session,
                tokenizer,
                dimension,
                model_name: model_name.to_string(),
                max_length: 512,
            })
        }

        /// Tokenize text and return (input_ids, attention_mask, token_type_ids).
        fn tokenize(&self, text: &str) -> Result<(Vec<i64>, Vec<i64>, Vec<i64>), InferenceError> {
            let encoding = self.tokenizer.encode(text, true).map_err(|e| {
                InferenceError::Other(format!("Tokenization failed: {e}"))
            })?;

            let mut input_ids: Vec<i64> = encoding.get_ids().iter().map(|&id| id as i64).collect();
            let mut attention_mask: Vec<i64> =
                encoding.get_attention_mask().iter().map(|&m| m as i64).collect();
            let mut token_type_ids: Vec<i64> =
                encoding.get_type_ids().iter().map(|&t| t as i64).collect();

            // Truncate to max_length
            input_ids.truncate(self.max_length);
            attention_mask.truncate(self.max_length);
            token_type_ids.truncate(self.max_length);

            Ok((input_ids, attention_mask, token_type_ids))
        }

        /// Mean pooling over token embeddings with attention mask weighting.
        fn mean_pool(
            token_embeddings: &ndarray::ArrayView2<f32>,
            attention_mask: &[i64],
        ) -> Vec<f32> {
            let dim = token_embeddings.ncols();
            let mut pooled = vec![0.0f32; dim];
            let mut total_weight = 0.0f32;

            for (i, &mask) in attention_mask.iter().enumerate() {
                if i >= token_embeddings.nrows() {
                    break;
                }
                let weight = mask as f32;
                total_weight += weight;
                for j in 0..dim {
                    pooled[j] += token_embeddings[[i, j]] * weight;
                }
            }

            if total_weight > 0.0 {
                for v in &mut pooled {
                    *v /= total_weight;
                }
            }

            pooled
        }

        /// L2-normalize a vector in place.
        fn l2_normalize(vec: &mut [f32]) {
            let norm: f32 = vec.iter().map(|v| v * v).sum::<f32>().sqrt();
            if norm > 1e-12 {
                for v in vec.iter_mut() {
                    *v /= norm;
                }
            }
        }
    }

    impl OnnxEmbeddingProvider {
        /// Synchronous embed — runs tokenization + ONNX inference on the current thread.
        /// Must be called from a blocking context (e.g., `spawn_blocking`).
        fn embed_sync(&self, text: &str) -> Result<Vec<f32>, InferenceError> {
            let (input_ids, attention_mask, token_type_ids) = self.tokenize(text)?;
            let seq_len = input_ids.len();

            // Create input tensors [1, seq_len]
            let ids_array = Array2::from_shape_vec((1, seq_len), input_ids.clone())
                .map_err(|e| InferenceError::Other(format!("Array shape error: {e}")))?;
            let mask_array = Array2::from_shape_vec((1, seq_len), attention_mask.clone())
                .map_err(|e| InferenceError::Other(format!("Array shape error: {e}")))?;
            let type_array = Array2::from_shape_vec((1, seq_len), token_type_ids)
                .map_err(|e| InferenceError::Other(format!("Array shape error: {e}")))?;

            // Run ONNX inference (CPU-bound, ~5ms for BGE-small-zh)
            let outputs = self
                .session
                .run(ort::inputs![ids_array, mask_array, type_array].map_err(|e| {
                    InferenceError::Other(format!("ONNX input error: {e}"))
                })?)
                .map_err(|e| InferenceError::Other(format!("ONNX inference error: {e}")))?;

            // Extract output tensor: [1, seq_len, hidden_size]
            let output = outputs.first().ok_or_else(|| {
                InferenceError::Other("No output from ONNX model".into())
            })?;

            let tensor = output.try_extract_tensor::<f32>().map_err(|e| {
                InferenceError::Other(format!("Failed to extract output tensor: {e}"))
            })?;

            // Handle both 2D [1, hidden_size] and 3D [1, seq_len, hidden_size] outputs
            let mut pooled = if tensor.ndim() == 3 {
                let view = tensor.view();
                let shape = view.shape();
                let token_emb = view
                    .into_shape_with_order((shape[1], shape[2]))
                    .map_err(|e| InferenceError::Other(format!("Reshape error: {e}")))?;
                Self::mean_pool(&token_emb, &attention_mask)
            } else if tensor.ndim() == 2 {
                // Already pooled by the model
                tensor.index_axis(Axis(0), 0).to_vec()
            } else {
                return Err(InferenceError::Other(format!(
                    "Unexpected output tensor dimensions: {}",
                    tensor.ndim()
                )));
            };

            Self::l2_normalize(&mut pooled);
            Ok(pooled)
        }
    }

    #[async_trait]
    impl EmbeddingProvider for OnnxEmbeddingProvider {
        async fn embed(&self, text: &str) -> Result<Vec<f32>, InferenceError> {
            // ONNX session.run() is a CPU-bound blocking call.
            // Wrapping in spawn_blocking would require Send + 'static for self,
            // which conflicts with the &self borrow. Instead, since the call is
            // ~5ms (BGE-small-zh), we accept the brief blocking. For models >100ms,
            // consider Arc<Self> + spawn_blocking.
            //
            // Note: this is the same pattern used by sensevoice.rs in this crate.
            self.embed_sync(text)
        }

        async fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, InferenceError> {
            // For simplicity, process one-by-one. Batch ONNX inference requires
            // padding to uniform length which adds complexity. At ~5ms per embed
            // for BGE-small-zh, sequential processing is adequate.
            let mut results = Vec::with_capacity(texts.len());
            for text in texts {
                results.push(self.embed(text).await?);
            }
            Ok(results)
        }

        fn dimension(&self) -> usize {
            self.dimension
        }

        fn name(&self) -> &str {
            &self.model_name
        }
    }
}

#[cfg(feature = "onnx")]
pub use onnx_impl::OnnxEmbeddingProvider;
