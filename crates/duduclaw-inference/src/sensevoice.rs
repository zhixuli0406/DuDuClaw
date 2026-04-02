//! SenseVoice ASR provider — local ONNX-based Chinese-optimized ASR.
//!
//! SenseVoice (Alibaba FunASR) achieves <5% WER on Mandarin benchmarks,
//! outperforming Whisper on CJK languages. Non-autoregressive architecture
//! means inference is ~15x faster than Whisper.
//!
//! Requires the `onnx` feature flag and a SenseVoice ONNX model file.
//! Model files are stored in `~/.duduclaw/models/sensevoice/`.

#[cfg(feature = "onnx")]
use ort::session::Session;

use async_trait::async_trait;
use tracing::{info, warn};

use crate::asr::AsrProvider;
use crate::error::InferenceError;

/// SenseVoice local ASR provider (ONNX Runtime).
pub struct SenseVoiceProvider {
    #[cfg(feature = "onnx")]
    session: Session,
    #[cfg(not(feature = "onnx"))]
    _phantom: (),
    model_path: String,
}

impl SenseVoiceProvider {
    /// Load a SenseVoice ONNX model from the given path.
    ///
    /// Supported models:
    /// - `sensevoice-small.onnx` (~200MB, recommended)
    /// - `sensevoice-large.onnx` (~800MB, best quality)
    pub fn load(model_path: &str) -> Result<Self, InferenceError> {
        // Path traversal protection
        if model_path.contains("..") || model_path.contains('\0') {
            return Err(InferenceError::Other(format!("Unsafe model path: {model_path}")));
        }

        #[cfg(feature = "onnx")]
        {
            info!(model = model_path, "Loading SenseVoice ONNX model");
            let session = Session::builder()
                .map_err(|e| InferenceError::Other(format!("ONNX session builder: {e}")))?
                .with_intra_threads(4)
                .map_err(|e| InferenceError::Other(format!("ONNX threads: {e}")))?
                .commit_from_file(model_path)
                .map_err(|e| InferenceError::Other(format!("Load SenseVoice model: {e}")))?;

            info!(
                inputs = session.inputs.len(),
                outputs = session.outputs.len(),
                "SenseVoice model loaded"
            );

            Ok(Self {
                session,
                model_path: model_path.to_string(),
            })
        }

        #[cfg(not(feature = "onnx"))]
        {
            let _ = model_path;
            Err(InferenceError::Other(
                "SenseVoice requires the 'onnx' feature flag".into(),
            ))
        }
    }

    /// Check if ONNX support is compiled in.
    pub fn is_available() -> bool {
        cfg!(feature = "onnx")
    }
}

#[async_trait]
impl AsrProvider for SenseVoiceProvider {
    async fn transcribe(&self, pcm_data: &[f32], lang: &str) -> Result<String, InferenceError> {
        #[cfg(feature = "onnx")]
        {
            use ort::value::Value;
            use std::sync::Arc;

            if pcm_data.is_empty() {
                return Err(InferenceError::Other("Empty audio input".into()));
            }

            let num_samples = pcm_data.len();
            let audio_tensor = Value::from_array(
                ort::memory::Allocator::default()
                    .map_err(|e| InferenceError::Other(format!("Allocator: {e}")))?,
                &ndarray_shape(pcm_data)?,
            )
            .map_err(|e| InferenceError::Other(format!("Audio tensor: {e}")))?;

            // Language ID mapping for SenseVoice
            let lang_id = match lang {
                "zh" | "cmn" | "zh-TW" | "zh-CN" => 0i64,
                "en" => 1,
                "ja" => 2,
                "ko" => 3,
                _ => 0, // Default to Chinese
            };

            let lang_tensor = Value::from_array(
                ort::memory::Allocator::default()
                    .map_err(|e| InferenceError::Other(format!("Allocator: {e}")))?,
                &ndarray::Array1::from_vec(vec![lang_id]).into_dyn(),
            )
            .map_err(|e| InferenceError::Other(format!("Lang tensor: {e}")))?;

            info!(
                samples = num_samples,
                duration_secs = num_samples as f32 / 16000.0,
                lang,
                "SenseVoice: transcribing"
            );

            let outputs = self
                .session
                .run(ort::inputs![audio_tensor, lang_tensor]
                    .map_err(|e| InferenceError::Other(format!("Inputs: {e}")))?)
                .map_err(|e| InferenceError::Other(format!("SenseVoice inference: {e}")))?;

            // Extract text from output tensor
            let text = outputs
                .get(0)
                .ok_or_else(|| InferenceError::Other("No output tensor".into()))?
                .try_extract_string_tensor()
                .map_err(|e| InferenceError::Other(format!("Extract text: {e}")))?;

            let result = text.iter().next().map(|s| s.to_string()).unwrap_or_default();
            info!(text_len = result.len(), "SenseVoice: transcription complete");
            Ok(result.trim().to_string())
        }

        #[cfg(not(feature = "onnx"))]
        {
            let _ = (pcm_data, lang);
            Err(InferenceError::Other(
                "SenseVoice requires the 'onnx' feature flag".into(),
            ))
        }
    }

    fn name(&self) -> &str {
        "sensevoice"
    }
}

#[cfg(feature = "onnx")]
fn ndarray_shape(pcm: &[f32]) -> Result<ndarray::ArrayD<f32>, crate::error::InferenceError> {
    ndarray::Array2::from_shape_vec((1, pcm.len()), pcm.to_vec())
        .map(|a| a.into_dyn())
        .map_err(|e| crate::error::InferenceError::Other(format!("ndarray shape: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn availability_matches_feature() {
        // In test builds without onnx feature, should return false
        let available = SenseVoiceProvider::is_available();
        #[cfg(feature = "onnx")]
        assert!(available);
        #[cfg(not(feature = "onnx"))]
        assert!(!available);
    }
}
